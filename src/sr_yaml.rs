use std::collections::HashMap;
use std::fs::File;
use std::io::BufReader;
use std::path::PathBuf;

use reqwest::blocking::Client;
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde::Serialize;
use serde_with::skip_serializing_none;
use url::Url;

use lib_sr;
use lib_sr::errors::*;
use lib_sr::event;

use crate::json_schema;

#[skip_serializing_none]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Step {
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
    pub labels: Option<Vec<String>>,
    pub run: Option<String>,
    #[serde(alias = "run-embedded", rename(serialize = "run-embedded"))]
    pub run_embedded: Option<String>,
    pub uses: Option<String>,
    #[serde(alias = "url")]
    uri: Option<String>,
}

#[skip_serializing_none]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Flow {
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
    pub steps: Option<Vec<Step>>,
    #[serde(alias = "url")]
    uri: Option<String>,
}

#[skip_serializing_none]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Label {
    #[serde(flatten)]
    extra: HashMap<String, serde_json::Value>,
    #[serde(alias = "json-schema", rename(serialize = "json-schema"))]
    json_schema: Option<serde_json::Value>,
    #[serde(
        alias = "json_schema_url",
        alias = "json-schema-uri",
        alias = "json-schema-url",
        rename(serialize = "json-schema-uri")
    )]
    json_schema_uri: Option<String>,
    question: Option<String>,
    required: Option<bool>,
    #[serde(alias = "url")]
    uri: Option<String>,
}

#[skip_serializing_none]
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Config {
    #[serde(
        alias = "base_url",
        alias = "base-uri",
        alias = "base-url",
        rename(serialize = "base-uri")
    )]
    pub base_uri: Option<String>,
    pub db: Option<String>,
    #[serde(flatten)]
    pub extra: HashMap<String, serde_json::Value>,
    pub flows: Option<HashMap<String, Flow>>,
    pub labels: Option<HashMap<String, Label>>,
    pub reviewer: Option<String>,
    #[serde(alias = "sink-all-events", rename(serialize = "sink-all-events"))]
    pub sink_all_events: Option<bool>,
}

impl Config {
    fn merge(mut self, other: Config) -> Self {
        self.extra.extend(other.extra.into_iter());
        Self {
            base_uri: other.base_uri.or(self.base_uri),
            db: other.db.or(self.db),
            extra: self.extra,
            flows: other.flows.or(self.flows),
            labels: other.labels.or(self.labels),
            reviewer: other.reviewer.or(self.reviewer),
            sink_all_events: other.sink_all_events.or(self.sink_all_events),
        }
    }
}

pub fn non_blank(id: &str, k: &str, s: &Option<String>) -> Result<String> {
    match s {
        Some(s) => {
            let s = s.trim();
            if s.is_empty() {
                Err(format!("The {} label has a blank {}", id, k).into())
            } else {
                Ok(s.to_string())
            }
        }
        None => Err(format!("The {} label does not have a {}", id, k).into()),
    }
}

pub fn get_object<T: DeserializeOwned>(client: &Client, url: &str) -> Result<T> {
    let response = client
        .get(url)
        .send()
        .chain_err(|| format!("Error while retrieving URL: {}", url))?;
    let status = response.status().as_u16();
    let text = response
        .text()
        .chain_err(|| "Error getting response text")?;
    if status == 200 {
        match serde_json::from_str(&text) {
            Ok(v) => Ok(v),
            Err(_) => serde_yaml::from_str(&text),
        }
        .chain_err(|| "Could not parse reponse")
    } else {
        Err(format!(
            "Unexpected {} status response at {} ({})",
            status, &url, text
        )
        .into())
    }
}

pub fn parse_step_data(step: Step) -> Result<lib_sr::Step> {
    let run_embedded = match step.uses {
        Some(s) => {
            let mut cmd = "run-using ".to_string();
            cmd.push_str(&s);
            Some(cmd)
        }
        None => step.run_embedded,
    };
    Ok(lib_sr::Step {
        extra: step.extra,
        labels: step.labels.unwrap_or(Vec::new()),
        run: step.run,
        run_embedded,
    })
}

pub fn parse_step(client: &Client, step: Step) -> Result<lib_sr::Step> {
    match &step.uri {
        Some(uri) => {
            let stp: Step = get_object(client, uri)?;
            parse_step_data(stp)
        }
        None => parse_step_data(step),
    }
}

pub fn parse_flow_data(client: &Client, flow: Flow) -> Result<lib_sr::Flow> {
    let steps = &mut flow.steps.unwrap_or(Vec::new());
    if steps.len() == 0 {
        return Err("No steps in flow".into());
    }

    let mut vec = Vec::new();
    for step in steps {
        let step = parse_step(client, step.to_owned())?;
        vec.push(step);
    }
    vec.push(lib_sr::Step {
        extra: HashMap::new(),
        labels: Vec::new(),
        run: None,
        run_embedded: Some(String::from("sink")),
    });
    Ok(lib_sr::Flow {
        extra: flow.extra,
        steps: vec,
    })
}

pub fn parse_flow(client: &Client, flow: Flow) -> Result<lib_sr::Flow> {
    match &flow.uri {
        Some(uri) => {
            let flw: Flow = get_object(client, uri)?;
            parse_flow_data(client, flw)
        }
        None => parse_flow_data(client, flow),
    }
}

pub fn parse_flows(
    client: &Client,
    flows: Option<HashMap<String, Flow>>,
) -> Result<HashMap<String, lib_sr::Flow>> {
    let flows = flows.unwrap_or(HashMap::new());
    let mut m = HashMap::new();
    for (flow_name, flow) in flows {
        let flow = parse_flow(client, flow)?;
        m.insert(flow_name, flow);
    }
    Ok(m)
}

pub fn parse_label_data(
    id: &str,
    label: &Label,
    json_schema: Option<serde_json::Value>,
) -> Result<lib_sr::Label> {
    let mut label = lib_sr::Label {
        extra: label.extra.clone(),
        hash: None,
        id: id.to_string(),
        json_schema,
        question: non_blank(id, "question", &label.question)?,
        required: label.required.unwrap_or(false),
    };
    let data_s = serde_json::to_string(&label).chain_err(|| "Serialization failed")?;
    let data = serde_json::from_str(&data_s).chain_err(|| "Deserialization failed")?;
    let event = event::Event {
        data: data,
        extra: HashMap::new(),
        hash: None,
        r#type: String::from("label"),
        uri: None,
    };
    label.hash = Some(event::event_hash(event)?);
    Ok(label)
}

pub fn parse_label(
    client: &Client,
    id: &str,
    label: &Label,
    json_schema: Option<serde_json::Value>,
) -> Result<lib_sr::Label> {
    match &label.uri {
        Some(uri) => {
            let lbl: Label = get_object(client, uri)?;
            parse_label_data(id, &lbl, json_schema)
        }
        None => parse_label_data(id, label, json_schema),
    }
}

lazy_static! {
    static ref SCHEMA_ALIASES: HashMap<&'static str, &'static str> = hashmap! {
        "boolean" => "https://docs.sysrev.com/schema/label-answer/boolean-v2.json",
        "string" => "https://docs.sysrev.com/schema/label-answer/string-v2.json",
    };
}

fn get_label_schema_for_alias(client: &Client, id: &str, alias: &str) -> Result<serde_json::Value> {
    match SCHEMA_ALIASES.get(alias) {
        Some(url) => json_schema::get_schema_for_url(client, url),
        None => Err(format!(
            "The {} label has an unknown value for json_schema: {}",
            id, alias
        )
        .into()),
    }
}

pub fn get_label_schema(
    client: &Client,
    label: &Label,
    id: &str,
) -> Result<Option<serde_json::Value>> {
    let json = if label.json_schema.is_some() {
        let lbl = label.json_schema.to_owned().unwrap();
        match lbl.as_str() {
            Some(s) => Some(get_label_schema_for_alias(client, id, s)?),
            None => Some(lbl),
        }
    } else {
        label
            .json_schema_uri
            .as_ref()
            .map(|url| json_schema::get_schema_for_url(client, &url))
            .transpose()?
    };
    // Fail fast if an invalid schema is supplied
    match json.clone() {
        Some(v) => Some(json_schema::compile(&v)?),
        None => None,
    };
    Ok(json)
}

pub fn parse_labels(
    client: &Client,
    labels: &Option<HashMap<String, Label>>,
) -> Result<HashMap<String, lib_sr::Label>> {
    match labels {
        Some(labels) => {
            let mut m = HashMap::new();
            for (id, label) in labels {
                let json_schema = get_label_schema(&client, &label, id)?;
                let parsed = parse_label(&client, &id, label, json_schema)?;
                m.insert(id.to_owned(), parsed);
            }
            Ok(m)
        }
        None => Ok(HashMap::new()),
    }
}

pub fn parse_config(config: Config) -> Result<lib_sr::Config> {
    let client = Client::new();
    let config = match &config.base_uri {
        Some(uri) => {
            let cfg: Config = get_object(&client, uri)?;
            cfg.merge(config)
        }
        None => config,
    };
    let reviewer = config.reviewer.ok_or("\"reviewer\" not set in config")?;
    let mut reviewer_err = String::from("\"reviewer\" is not a valid URI: ");
    reviewer_err.push_str(&format!("{:?}", reviewer));
    let reviewer_uri = Url::parse(&reviewer);
    match reviewer_uri {
        Ok(_) => Ok(lib_sr::Config {
            current_labels: None,
            current_step: None,
            db: config.db.unwrap_or(String::from("sink.jsonl")),
            extra: config.extra,
            flows: parse_flows(&client, config.flows)?,
            labels: parse_labels(&client, &config.labels)?,
            reviewer,
            sink_all_events: config.sink_all_events.unwrap_or(false),
            srvc: lib_sr::Srvc {
                version: String::from(env!("CARGO_PKG_VERSION")),
            },
        }),
        Err(_) => {
            if !reviewer.contains(":") && reviewer.contains("@") && reviewer.contains(".") {
                let mut try_reviewer = String::from("mailto:");
                try_reviewer.push_str(&reviewer);
                reviewer_err.push_str(&format!("\n  Try {:?}", try_reviewer));
            }
            Err(reviewer_err.into())
        }
    }
}

pub fn get_config(filename: PathBuf) -> Result<Config> {
    let file = File::open(filename.clone())
        .chain_err(|| format!("Failed to open config file: {}", filename.to_string_lossy()))?;
    let reader = BufReader::new(file);
    serde_yaml::from_reader(reader).chain_err(|| {
        format!(
            "Failed to parse config file as YAML: {}",
            filename.to_string_lossy()
        )
    })
}
