use anyhow::Result;
use tg_ws_proxy_plus::config::{Config, normalize_domains};

pub fn parse(value: serde_json::Value, current: &Config) -> Result<Config> {
    let mut config: Config = serde_json::from_value(value)?;
    config.extra = current.extra.clone();
    config.secret = config.secret.trim().to_ascii_lowercase();
    config.host = config.host.trim().into();
    config.fake_tls_domain = config.fake_tls_domain.trim().to_ascii_lowercase();
    config.dc_ip = config
        .dc_ip
        .iter()
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect();
    config.cfproxy_user_domain = normalize_domains(&config.cfproxy_user_domain);
    config.cfproxy_worker_domain = normalize_domains(&config.cfproxy_worker_domain);
    config.validate()?;
    Ok(config)
}

pub fn network_changed(old: &Config, new: &Config) -> Result<bool> {
    fn network(config: &Config) -> Result<serde_json::Value> {
        let mut value = serde_json::to_value(config)?;
        if let Some(object) = value.as_object_mut() {
            for name in [
                "autostart",
                "check_updates",
                "appearance",
                "language",
                "verbose",
                "log_max_mb",
                "log_backups",
            ] {
                object.remove(name);
            }
        }
        Ok(value)
    }
    Ok(network(old)? != network(new)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presentation_changes_keep_connections_alive() {
        let old = Config::fresh().unwrap();
        let mut new = old.clone();
        new.language = "en".into();
        new.appearance = "dark".into();
        new.autostart = true;
        new.verbose = true;
        new.log_max_mb = 8.0;
        assert!(!network_changed(&old, &new).unwrap());
        new.port += 1;
        assert!(network_changed(&old, &new).unwrap());
    }
    #[test]
    fn settings_are_validated_and_preserve_unknown_saved_fields() {
        let mut old = Config::fresh().unwrap();
        old.extra
            .insert("future_option".into(), serde_json::json!(42));
        let mut value = serde_json::to_value(&old).unwrap();
        value["port"] = serde_json::json!(0);
        assert!(parse(value, &old).is_err());
        let mut value = serde_json::to_value(&old).unwrap();
        value["future_option"] = serde_json::json!(99);
        value["dc_ip"] = serde_json::json!([" 2:149.154.167.220 ", ""]);
        let next = parse(value, &old).unwrap();
        assert_eq!(next.extra, old.extra);
        assert_eq!(next.dc_ip, ["2:149.154.167.220"]);
    }
}
