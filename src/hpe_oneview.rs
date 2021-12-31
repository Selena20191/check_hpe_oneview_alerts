use crate::constants;
use crate::json;
use crate::nagios;

use http::StatusCode;
use reqwest::{blocking, header, Certificate};
use serde_json::json;
use std::error::Error;

pub fn check_alerts(
    host: &str,
    user: &str,
    pass: &str,
    ca: &[u8],
    insecure: bool,
) -> Result<nagios::NagiosState, Box<dyn Error>> {
    let client = create_client(ca, insecure)?;
    let mut result = nagios::NagiosState {
        status: nagios::UNKNOWN,
        message: String::new(),
    };
    let session_token = login(&client, host, user, pass)?;
    let alerts = get_alerts(&client, host, &session_token)?;
    let mut ok_count: u64 = 0;
    let mut warn_count: u64 = 0;
    let mut critical_count: u64 = 0;
    let mut msg_list = Vec::<String>::new();

    // No alerts? HAPPY! HAPPY! JOY! JOY!
    if alerts.count == 0 {
        return Ok(nagios::NagiosState {
            status: nagios::OK,
            message: "No uncleared alerts found".to_string(),
        });
    }

    // Loop over alerts
    for alert in alerts.members {
        match alert.severity.to_lowercase().as_str() {
            "ok" => ok_count += 1,
            "warning" => warn_count += 1,
            "critical" => critical_count += 1,
            _ => {
                bail!("BUG: Unknown alert severity {}", alert.severity);
            }
        };
    }

    if critical_count > 0 {
        result.status = nagios::CRITICAL;
    } else if warn_count > 0 {
        result.status = nagios::WARNING;
    } else {
        result.status = nagios::OK;
    }

    if critical_count > 0 {
        msg_list.push(format!("{} critical alerts found", critical_count))
    }
    if warn_count > 0 {
        msg_list.push(format!("{} warning alerts found", warn_count));
    }
    msg_list.push(format!("{} harmless alerts found", ok_count));

    result.message = msg_list.join(", ");

    // We don't give a shit if the logout fails
    #[allow(unused_must_use)]
    {
        logout(&client, host, &session_token);
    }
    Ok(result)
}

fn login(
    client: &reqwest::blocking::Client,
    host: &str,
    user: &str,
    pass: &str,
) -> Result<String, Box<dyn Error>> {
    let payload = json!({
        "userName": user,
        "password": pass,
    })
    .to_string();

    let request = client
        .post(format!("https://{}/rest/login-sessions", host))
        .body(payload)
        .send()?;

    // Note: For invalid logins, HPE OneView returns **200 OK** but sets not sessionID
    let result_headers = request.headers();
    let token = match result_headers.get("sessionID") {
        Some(v) => v.to_str()?.to_string(),
        None => bail!("Login to HPE OneView failed"),
    };

    Ok(token)
}

fn logout(
    client: &reqwest::blocking::Client,
    host: &str,
    token: &str,
) -> Result<(), Box<dyn Error>> {
    let session = json!({
        "Auth": token,
    })
    .to_string();

    client
        .delete(format!("https://{}/rest/login-sessions", host))
        .body(session)
        .send()?;

    Ok(())
}

fn get_alerts(
    client: &reqwest::blocking::Client,
    host: &str,
    token: &str,
) -> Result<json::AlertResourceCollection, Box<dyn Error>> {
    let session = json!({
        "Auth": token,
    })
    .to_string();

    let request = client
        .get(format!(
            "https://{}/rest/alerts?filter=%%22alertState<>%%27Cleared%%27%%22",
            host
        ))
        .body(session)
        .send()?;

    if request.status() != StatusCode::OK {
        bail!(request
            .status()
            .canonical_reason()
            .unwrap_or("unknown HTTP status"));
    }

    let reply = request.text()?;
    let alerts = serde_json::from_str(&reply)?;

    Ok(alerts)
}

fn create_client(
    ca_cert: &[u8],
    insecure_ssl: bool,
) -> Result<reqwest::blocking::Client, Box<dyn Error>> {
    let mut cli = blocking::ClientBuilder::new().use_native_tls();
    let user_agent = constants::generate_user_agent();

    let mut head = header::HeaderMap::new();
    head.insert(
        header::ACCEPT,
        header::HeaderValue::from_str("application/json").unwrap(),
    );
    head.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_str("application/json").unwrap(),
    );
    head.insert(
        header::USER_AGENT,
        header::HeaderValue::from_str(&user_agent).unwrap(),
    );
    head.insert(
        "X-Api-Version",
        header::HeaderValue::from_str(constants::HPE_ONEVIEW_API_VERSION).unwrap(),
    );

    if insecure_ssl {
        cli = cli.danger_accept_invalid_certs(true);
        cli = cli.danger_accept_invalid_hostnames(true);
    } else if !ca_cert.is_empty() {
        // Only add CA if insecure_ssl is false (would be silly to do otherwise)
        let ca = Certificate::from_pem(ca_cert)?;
        cli = cli.add_root_certificate(ca);
    }

    cli = cli.default_headers(head);

    // Disable idle pool, some management boards don't like connection reuse.
    cli = cli.pool_max_idle_per_host(0);

    let res = cli.build().unwrap();

    Ok(res)
}
