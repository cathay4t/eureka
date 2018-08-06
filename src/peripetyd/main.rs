extern crate peripety;
extern crate regex;
extern crate serde_json;
#[macro_use]
extern crate serde_derive;
extern crate toml;

#[macro_use]
mod misc;
mod buildin_regex;
mod conf;
mod data;
//mod fs;
//mod mpath;
//mod scsi;

use buildin_regex::BUILD_IN_REGEX_CONFS;
use data::RegexConf;
use peripety::{BlkInfo, StorageSubSystem};
use serde_json::{Map, Value};
use std::io::Write;

fn main() {
    let mut regex_confs: Vec<RegexConf> = Vec::new();

    if let Some(c) = conf::load_conf() {
        for regex_conf in c.regexs {
            match regex_conf.to_regex_conf() {
                Ok(r) => regex_confs.push(r),
                Err(e) => {
                    to_stderr!("Invalid regex configuration: {}", e);
                    continue;
                }
            }
        }
    }

    for regex_conf_str in BUILD_IN_REGEX_CONFS {
        regex_confs.push(regex_conf_str.to_regex_conf());
    }

    let fd = std::io::stdin();
    let mut input = String::new();
    loop {
        input.clear();
        match fd.read_line(&mut input) {
            Ok(_) => {
                let line = input.trim();
                if line.is_empty() {
                    continue;
                }
                if let Some(event_str) = parse_rsyslog_log(&line, &regex_confs)
                {
                    to_stdout!("{}", event_str);
                } else {
                    to_stdout!("{}", "{}".to_string());
                    // ^ empty json means no filed is to be modified.
                }
            }
            Err(error) => {
                to_stderr!("Got error when reading from stdin: {}", error)
            }
        }
    }
}

fn parse_rsyslog_log(
    json_str: &str,
    regex_confs: &Vec<RegexConf>,
) -> Option<String> {
    let v: Value = match serde_json::from_str(json_str) {
        Ok(v) => v,
        Err(e) => {
            to_stderr!(
                "Error: Failed to parse json string: {}, error: {}",
                json_str,
                e
            );
            return None;
        }
    };
    let mut matched = false;
    let mut event = Map::new();
    let json_obj = match v.as_object() {
        Some(m) => m,
        None => {
            to_stderr!("Error: JSON data is not a object : {}", json_str);
            return None;
        }
    };

    // Skip messages generated by peripetyd.
    if json_obj.get("structured-data") != Some(&Value::String("-".to_string()))
    {
        return None;
    }

    // Skip non-kernel messages
    if json_obj.get("syslogfacility") != Some(&Value::String("0".to_string())) {
        return None;
    }

    let msg = match json_obj.get("msg").and_then(|m| m.as_str()) {
        Some(m) => m.to_string(),
        None => {
            to_stderr!("Error: JSON data is missing 'msg' data: {}", json_str);
            return None;
        }
    };
    let mut new_msg = msg.clone();
    let msg = if msg.starts_with('[') {
        if let Some(i) = msg.find(']') {
            &msg[i + 2..]
        } else {
            &msg
        }
    } else {
        &msg
    };

    for regex_conf in regex_confs {
        // Save CPU from regex.captures() if starts_with() failed.
        if let Some(ref s) = regex_conf.starts_with {
            if !msg.starts_with(s) {
                continue;
            }
        }

        if let Some(cap) = regex_conf.regex.captures(&msg) {
            if let Some(m) = cap.name("kdev") {
                let kdev = m.as_str();
                if let Ok(bi) = BlkInfo::new(kdev) {
                    if let Ok(s) = bi.to_json_string() {
                        matched = true;
                        event.insert(
                            "BLK_INFO_JSON".to_string(),
                            Value::String(s),
                        );
                        if !bi.preferred_blk_path.is_empty() {
                            event.insert(
                                "BLK_PATH".to_string(),
                                Value::String(bi.preferred_blk_path),
                            );
                        }
                        if !bi.wwid.is_empty() {
                            new_msg = format!("{} wwid: {}", new_msg, &bi.wwid);
                            event.insert(
                                "WWID".to_string(),
                                Value::String(bi.wwid),
                            );
                        }
                        if let Some(u) = bi.uuid {
                            new_msg = format!("{} uuid: {}", new_msg, &u);
                            event.insert("UUID".to_string(), Value::String(u));
                        }
                        if let Some(mp) = bi.mount_point {
                            new_msg =
                                format!("{} mount_point: {}", new_msg, &mp);
                            event.insert(
                                "MOUNT_POINT".to_string(),
                                Value::String(mp),
                            );
                        }
                        if !bi.transport_id.is_empty() {
                            event.insert(
                                "TRANSPORT_ID".to_string(),
                                Value::String(bi.transport_id),
                            );
                        }
                    } else {
                        continue;
                    }
                } else {
                    continue;
                }
            }

            if regex_conf.sub_system != StorageSubSystem::Unknown {
                event.insert(
                    "SUB_SYSTEM".to_string(),
                    Value::String(regex_conf.sub_system.to_string()),
                );
            }

            if !regex_conf.event_type.is_empty() {
                let event_type_str = regex_conf.event_type.to_string();
                new_msg =
                    format!("{} event_type: {}", new_msg, &event_type_str);
                event.insert(
                    "EVENT_TYPE".to_string(),
                    Value::String(event_type_str),
                );
            }

            // If regex has other named group, we save it to
            // event.extension.
            for name in regex_conf.regex.capture_names() {
                if let Some(name) = name {
                    if name == "kdev" {
                        continue;
                    }
                    if let Some(v) = cap.name(name) {
                        event.insert(
                            name.to_string(),
                            Value::String(v.as_str().to_string()),
                        );
                    }
                }
            }

            break;
        }
    }
    // Currently kernel does not provides sufficient structured data,
    // Since we will do regex match anyway, there is no need to parse
    // "SUBSYSTEM" and "DEVICE" entries for SCSI logs.
    if matched {
        let mut ret = Map::new();

        ret.insert("msg".to_string(), Value::String(new_msg));
        match serde_json::to_string(&event) {
            Ok(s) => {
                ret.insert("structured-data".to_string(), Value::String(s))
            }
            Err(e) => return Some(format!(r#"{{"error": "{}"}}"#, e)),
        };

        match serde_json::to_string(&ret) {
            Ok(s) => {
                return Some(s);
            }
            Err(e) => return Some(format!(r#"{{"error": "{}"}}"#, e)),
        };
    }
    None
}
