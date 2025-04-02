use parking_lot::Mutex;
use serde_json::to_string_pretty;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::env::VarError;
use std::process;
use std::sync::Arc;
use tracing::warn;

use chrono::Utc;
use clap::Parser;
use serde::{Deserialize, Serialize};
use slack_bot::file::*;
use slack_bot::slack::*;
use slack_bot::utils::cronjob::*;
use slack_bot::utils::retry::*;
use slack_bot::utils::time::*;
use slack_bot::*;
use tracing::{Level, error};

const SAVE_DATA_PERIOD_MS: u64 = 600_000;
const SAVE_DATA_TIMEOUT_MS: u64 = 5_000;

// TODO: use compactstr to store user id

#[derive(Parser, Debug)]
#[command(version, about = "config file should contains a list of username in json format", long_about = None)]
struct Args {
    /// The record file name
    #[arg(short, long, value_name = "OUTPUT", default_value_t = String::from("record.json"))]
    output: String,

    /// The config file name
    #[arg(long, value_name = "CONFIG")]
    config: String,

    /// The config file name
    #[arg(long, value_name = "SLACK_BOT_TOKEN")]
    token: Option<String>,

    /// The testing count
    #[arg(short, long, default_value_t = 5)]
    count: usize,

    /// The testing period (day)
    #[arg(short, long, default_value_t = 10)]
    day: usize,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    let args = Args::parse();

    let token = if let Some(token) = args.token {
        token
    } else {
        match std::env::var(SLACK_TOKEN_ENV) {
            Ok(token) => token,
            Err(VarError::NotPresent) => {
                error!(
                    "cannot get slack bot token, please add `token` argument or add to env: {}",
                    SLACK_TOKEN_ENV
                );
                process::exit(0);
            }
            Err(err) => {
                error!("err: {}", err.to_string());
                process::exit(0);
            }
        }
    };
    let client = Arc::new(SlackClient::new(&token));

    // TODO: add parsing failed error msg
    let raw_user = match dump_file(&args.config).await {
        Ok(content) => content,
        Err(err) => panic!("read config file error: {}", err),
    };
    let user_list: Vec<String> = match serde_json::from_str(&raw_user) {
        Ok(content) => content,
        Err(err) => panic!("config file format is invalid: {}", err),
    };

    let data: Arc<Mutex<HashMap<String, i64>>> = Arc::default();
    let cronjob = {
        let data = data.clone();
        let filename = args.output.clone();

        let save_task_maker = move || {
            let data = data.clone();
            let filename = filename.clone();
            async move {
                let output = to_string_pretty(&Record::from_iter(data.lock().iter())).unwrap();
                if let Err(err) = tokio::fs::write(&filename, &output).await {
                    error!("save data failed: {err}");
                }
            }
        };

        AsyncCronJob::new(save_task_maker, SAVE_DATA_PERIOD_MS, SAVE_DATA_TIMEOUT_MS)
    };

    // start slack bot task for all user
    let mut handlers = Vec::new();
    user_list.into_iter().for_each(|user| {
        let data = data.clone();
        let client = client.clone();

        let handler = tokio::spawn(async move {
            slack_test(user, data, args.count, args.day, client).await;
        });

        handlers.push(handler);
    });

    // wait until all task ending...
    for handle in handlers {
        handle.await.ok();
    }

    if cronjob.shutdown().await.is_err() {
        warn!("failed to save records gracefullty...");
    }
}

#[derive(Deserialize, Serialize, Debug)]
struct Record {
    user_id: String,
    point: i64,
}

impl Record {
    pub fn from_iter<'a, I>(map: I) -> Vec<Self>
    where
        I: Iterator<Item = (&'a String, &'a i64)>,
    {
        map.map(|(id, point)| Record {
            user_id: id.clone(),
            point: *point,
        })
        .collect()
    }
}

enum Event {
    Emit(i64),
    Check(i64),
}

impl Event {
    fn get_ts(&self) -> i64 {
        match self {
            Self::Emit(ts) => *ts,
            Self::Check(ts) => *ts,
        }
    }
}

async fn slack_test(
    user_id: String,
    data: Arc<Mutex<HashMap<String, i64>>>,
    count: usize,
    days: usize,
    client: Arc<SlackClient>,
) {
    let curr_ts = Utc::now().timestamp(); // GMT

    // next day
    let start_ts = curr_ts / SEC_PER_DAY * SEC_PER_DAY + SEC_PER_DAY;
    let mut emit_ts: Vec<Event> = rand::random_iter::<u64>()
        .take(count)
        .flat_map(|rand_num| {
            // start + which day + which time
            // don't be too precisely at 17:00
            let ts = start_ts
                + SEC_PER_DAY * (rand_num % days as u64) as i64
                + rand::random_range(0..(8 * 3600));
            let emit = Event::Emit(ts);
            let check = Event::Check(ts + SEC_PER_DAY);
            [emit, check]
        })
        .collect();
    emit_ts.sort_by(|a, b| a.get_ts().cmp(&b.get_ts()));

    let mut post_resp = VecDeque::new();
    for event in emit_ts {
        let curr_ts = Utc::now().timestamp();
        match event {
            Event::Emit(ts) => {
                // sleep to emit time
                tokio::time::sleep(tokio::time::Duration::from_secs((ts - curr_ts) as u64)).await;
                let result = retry_res(3, 5000, || client.send_msg(&user_id, "hello")).await;
                match result {
                    Ok(resp) => post_resp.push_back(Some(resp)),
                    Err(err) => {
                        error!("slack pinging failed: {:?}", err);
                        post_resp.push_back(None);
                    }
                }
            }
            Event::Check(ts) => {
                tokio::time::sleep(tokio::time::Duration::from_secs((ts - curr_ts) as u64)).await;
                if let Some(msg_id) = post_resp.pop_front().unwrap() {
                    let result = retry_res(3, 5000, || client.get_reaction(&msg_id)).await;
                    match result {
                        Ok(resp) => {
                            if !resp.is_empty() {
                                client.send_msg(&user_id, "got msg!!!").await.ok();

                                let mut mapping = data.lock();
                                mapping
                                    .entry(user_id.clone())
                                    .and_modify(|val| *val += 1)
                                    .or_insert(1);
                            } else {
                                client
                                    .send_msg(&user_id, "cannot find your reaction QQ")
                                    .await
                                    .ok();
                            }
                        }
                        Err(err) => {
                            error!("slack get reaction failed: {:?}", err);
                        }
                    }
                }
            }
        }
    }
}
