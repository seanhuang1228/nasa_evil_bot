use parking_lot::Mutex;
use reqwest::Client;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::env::VarError;
use std::process;
use std::sync::Arc;
use tracing::info;

use chrono::Utc;
use clap::Parser;
use serde::{Deserialize, Serialize};
use tokio::{fs::File, io::AsyncReadExt};
use tracing::{Level, error};

static SLACK_TOKEN_ENV: &str = "SLACK_BOT_TOKEN";

lazy_static::lazy_static! {
    static ref CLIENT: Client = Client::new();
}
#[derive(Parser, Debug)]
#[command(version, about = "config file should contains a list of username in json format", long_about = None)]
struct Args {
    /// The record file name
    #[arg(short, long, value_name = "OUTPUT", default_value_t = String::from("record.json"))]
    output: String,

    /// The config file name
    #[arg(long, value_name = "CONFIG")]
    config: String,

    /// The testing count
    #[arg(short, long, default_value_t = 5)]
    count: i64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt().with_max_level(Level::INFO).init();

    if let Err(VarError::NotPresent) = std::env::var(SLACK_TOKEN_ENV) {
        error!("env not set: {}", SLACK_TOKEN_ENV);
        process::exit(0);
    }

    let args = Args::parse();

    let mut config = File::open(args.config.as_str()).await?;
    let mut content = String::new();
    config.read_to_string(&mut content).await?;
    drop(config);
    let user_list: Vec<String> = serde_json::from_str(&content)?;

    let mappings: Arc<Mutex<HashMap<String, i64>>> = Arc::default();
    let _guard = Guard::new(&args.output, mappings.clone());

    {
        let mappings = mappings.clone();
        let filename = args.output.clone();
        tokio::task::spawn(async move {
            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(3600)).await;
                let mut mappings = mappings.lock();
                let records: Vec<Record> = mappings
                    .drain()
                    .map(|(name, point)| Record {
                        user_id: name,
                        point,
                    })
                    .collect();

                info!("{records:?}");
                let serialized = serde_json::to_string_pretty(&records).unwrap();
                if let Err(err) = std::fs::write(&filename, serialized) {
                    println!("error: {err:?}");
                }
            }
        });
    }

    // let file = File::open("config.json")
    let mut handlers = Vec::new();
    for user in user_list {
        let data = mappings.clone();
        let user = user.clone();
        let count = args.count;
        handlers.push(tokio::spawn(async move {
            slack_test(user, data, count).await.ok();
        }));
    }

    for handle in handlers {
        handle.await.ok();
    }

    Ok(())
}

#[derive(Deserialize, Serialize, Debug)]
struct Record {
    user_id: String,
    point: i64,
}

struct Guard {
    filename: String,
    data: Arc<Mutex<HashMap<String, i64>>>,
}

impl Guard {
    fn new<T: Into<String>>(filename: T, data: Arc<Mutex<HashMap<String, i64>>>) -> Self {
        Guard {
            filename: filename.into(),
            data,
        }
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        let mut mapping = self.data.lock();
        let records: Vec<Record> = mapping
            .drain()
            .map(|(name, point)| Record {
                user_id: name,
                point,
            })
            .collect();

        info!("{records:?}");
        let serialized = serde_json::to_string_pretty(&records).unwrap();
        if let Err(err) = std::fs::write(&self.filename, serialized) {
            println!("error: {err:?}");
        }
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
    count: i64,
) -> Result<(), Box<dyn std::error::Error>> {
    const SEC_PER_DAY: i64 = 86400;
    let token = std::env::var("SLACK_BOT_TOKEN").unwrap();
    let curr_ts = Utc::now().timestamp(); // GMT

    // next day
    let start_ts = curr_ts / SEC_PER_DAY * SEC_PER_DAY + SEC_PER_DAY;
    let mut emit_ts: Vec<Event> = rand::random_iter::<u64>()
        .take(count as usize)
        .flat_map(|rand_num| {
            // start + which day + which time
            // don't be too precisely at 17:00
            let ts = start_ts + 86400 * (rand_num % 10) as i64 + rand::random_range(0..(8 * 3600));
            let emit = Event::Emit(ts);
            let check = Event::Check(ts + 86400);
            [emit, check]
        })
        .collect();

    emit_ts.sort_by(|a, b| a.get_ts().cmp(&b.get_ts()));

    let mut post_resp = VecDeque::new();
    for event in emit_ts {
        let curr_ts = Utc::now().timestamp();
        match event {
            Event::Emit(ts) => {
                info!("{ts} & {curr_ts}");
                tokio::time::sleep(tokio::time::Duration::from_secs((ts - curr_ts) as u64)).await;

                let ret: PostMessageResponse = CLIENT
                    .post("https://slack.com/api/chat.postMessage")
                    .header("Authorization", format!("Bearer {}", token))
                    .json(&serde_json::json!({
                        "channel": &user_id,
                        "text": "hello, please give me some reaction in 24hr",
                    }))
                    .send()
                    .await?
                    .json()
                    .await?;
                post_resp.push_back(ret);
            }
            Event::Check(ts) => {
                info!("{ts} & {curr_ts}");
                tokio::time::sleep(tokio::time::Duration::from_secs((ts - curr_ts) as u64)).await;

                let post_resp = post_resp.pop_front().unwrap();
                info!("{post_resp:?}");
                if post_resp.ok {
                    let reaction_url = format!(
                        "https://slack.com/api/reactions.get?channel={}&timestamp={}",
                        post_resp.channel, post_resp.ts
                    );

                    let reaction_response: ReactionsResponse = CLIENT
                        .get(&reaction_url)
                        .header("Authorization", format!("Bearer {}", token))
                        .send()
                        .await?
                        .json()
                        .await?;

                    if reaction_response.ok {
                        if let Some(msg) = reaction_response.message {
                            if !msg["reactions"].is_null() {
                                info!("get reaction");
                                let mut mapping = data.lock();
                                mapping
                                    .entry(user_id.clone())
                                    .and_modify(|val| *val += 1)
                                    .or_insert(1);
                            }
                        }
                    } else {
                        error!("Error fetching reactions: {}", user_id);
                    }
                } else {
                    error!("failed to send msg to: {}", user_id);
                }
            }
        }
    }

    Ok(())
}

#[derive(Deserialize, Debug)]
struct PostMessageResponse {
    ok: bool,
    channel: String,
    ts: String,
}

#[derive(Deserialize, Debug)]
struct ReactionItem {
    name: String,
    count: u32,
    users: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct ReactionsResponse {
    ok: bool,
    message: Option<serde_json::Value>,
}
