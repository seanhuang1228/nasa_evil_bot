use std::{fmt::Debug, str::FromStr};

use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug)]
pub enum SlackError {
    Request(String),
    Decode(reqwest::Error),
    Send(reqwest::Error),
    Other(&'static str),
}

pub struct SlackClient {
    client: Client,
    token: String,
}

impl SlackClient {
    pub fn new<T: Into<String>>(token: T) -> Self {
        Self {
            client: Client::new(),
            token: token.into(),
        }
    }

    pub async fn send_msg(
        &self,
        user_id: &str,
        msg: &str,
    ) -> Result<MessageIdentifier, SlackError> {
        let ret = self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .header("Authorization", format!("Bearer {}", self.token))
            .json(&serde_json::json!({
                "channel": user_id,
                "text": msg,
            }))
            .send()
            .await
            .map_err(|err| SlackError::Send(err))?
            .text()
            .await
            .map_err(|err| SlackError::Decode(err))?;

        let value = Value::from_str(&ret).unwrap();

        match value["ok"].as_bool() {
            Some(true) => Ok(MessageIdentifier {
                channel: value["channel"].as_str().unwrap().to_string(),
                ts: value["ts"].as_str().unwrap().to_string(),
            }),
            Some(false) => Err(SlackError::Request(
                value["error"].as_str().unwrap().to_string(),
            )),
            _ => Err(SlackError::Other("response without status code")),
        }
    }

    pub async fn get_reaction(
        &self,
        identifier: &MessageIdentifier,
    ) -> Result<Vec<Reaction>, SlackError> {
        let reaction_url = format!(
            "https://slack.com/api/reactions.get?channel={}&timestamp={}",
            identifier.channel, identifier.ts
        );

        let ret = self
            .client
            .get(&reaction_url)
            .header("Authorization", format!("Bearer {}", self.token))
            .send()
            .await
            .map_err(|err| SlackError::Send(err))?
            .text()
            .await
            .map_err(|err| SlackError::Decode(err))?;

        let value = Value::from_str(&ret).unwrap();

        match value["ok"].as_bool() {
            Some(true) => {
                if let Some(reac_values) = value["message"]["reactions"].as_array() {
                    let mut reactions = Vec::new();
                    for reac_value in reac_values {
                        let reaction: Reaction = serde_json::from_value(reac_value.clone())
                            .map_err(|_| SlackError::Other("reaction parsing error!"))?;
                        reactions.push(reaction);
                    }
                    Ok(reactions)
                } else {
                    Err(SlackError::Other("no reactions"))
                }
            }
            Some(false) => Err(SlackError::Request(
                value["error"].as_str().unwrap().to_string(),
            )),
            _ => Err(SlackError::Other("response without status code")),
        }
    }
}

#[allow(dead_code)]
#[derive(Deserialize)]
pub struct Reaction {
    name: String,
    users: Vec<String>,
}

pub struct MessageIdentifier {
    channel: String,
    ts: String,
}
