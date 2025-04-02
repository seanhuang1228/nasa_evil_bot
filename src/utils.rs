use chrono::Utc;
use tokio::spawn;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{Duration, sleep, timeout};

pub mod retry {
    use super::*;

    pub async fn retry_res<T, E, F>(
        count: usize,
        interval_ms: u64,
        future_maker: impl Fn() -> F,
    ) -> Result<T, E>
    where
        F: Future<Output = Result<T, E>>,
    {
        let mut ret = future_maker().await;
        for _ in 1..count {
            if ret.is_ok() {
                break;
            }
            sleep(Duration::from_millis(interval_ms)).await;
            ret = future_maker().await;
        }

        ret
    }
}

pub mod cronjob {
    use super::*;

    pub enum CronJobError {
        Timeout,
        TaskFailed,
    }

    enum Event {
        Emit,
        Terminate,
    }

    pub struct AsyncCronJob {
        timeout_ms: u64,
        event_tx: mpsc::UnboundedSender<Event>,
        ended_rx: oneshot::Receiver<()>,
    }

    impl AsyncCronJob {
        pub fn new<T, F>(task_maker: T, period_ms: u64, timeout_ms: u64) -> Self
        where
            T: Fn() -> F + Send + 'static,
            F: Future<Output = ()> + Send + 'static,
        {
            let (event_tx, mut event_rx) = mpsc::unbounded_channel();
            let (ended_tx, ended_rx) = oneshot::channel();

            // The timer to start the task periodically
            {
                let sender = event_tx.clone();
                spawn(async move {
                    let mut nxt_ts_ms = Utc::now().timestamp_millis() as u64;
                    loop {
                        let curr_ts_ms = Utc::now().timestamp_millis() as u64;
                        if curr_ts_ms >= nxt_ts_ms {
                            sender.send(Event::Emit).ok();
                            nxt_ts_ms += period_ms;
                        }

                        if curr_ts_ms > nxt_ts_ms {
                            panic!("curr timestamp is greater then next emit timestamp!");
                        }

                        sleep(Duration::from_millis((nxt_ts_ms - curr_ts_ms) as u64)).await;
                    }
                });
            }

            // Ther task runner
            spawn(async move {
                while let Some(event) = event_rx.recv().await {
                    task_maker().await;
                    match event {
                        Event::Emit => {}
                        Event::Terminate => {
                            break;
                        }
                    }
                }
                ended_tx.send(()).ok();
            });

            Self {
                timeout_ms,
                event_tx,
                ended_rx,
            }
        }

        pub async fn shutdown(self) -> Result<(), CronJobError> {
            self.event_tx.send(Event::Terminate).ok();
            timeout(Duration::from_millis(self.timeout_ms), self.ended_rx)
                .await
                .map_err(|_| CronJobError::Timeout)?
                .map_err(|_| CronJobError::TaskFailed)
        }
    }
}

pub mod time {
    pub const SEC_PER_DAY: i64 = 86400;
}
