#![feature(backtrace)]

use config::{
    Config,
    Environment,
};

use futures::{
    pin_mut,
    select,
    stream::{
        self,
        SelectAll,
    },
    StreamExt,
    TryStreamExt,
};

use log::*;
use rusoto_core::{
    region::Region,
    request::HttpClient,
    Client as AwsClient,
};

use rusoto_credential::AutoRefreshingProvider;
use rusoto_sqs::{
    DeleteMessageRequest,
    ReceiveMessageRequest,
    Sqs,
    SqsClient,
};

use rusoto_sts::WebIdentityProvider;
use serde::Deserialize;
use std::{
    env::var_os,
    net::SocketAddr,
};

use tokio::{
    net::TcpListener,
    signal::unix::{
        signal,
        SignalKind,
    },
    time::{
        delay_for,
        Duration,
    },
};

use tracing_subscriber::{
    fmt::Subscriber as TracingSubscriber,
    EnvFilter as TracingEnvFilter,
};

#[allow(dead_code)]
mod built_info;

#[derive(Debug, Deserialize)]
struct Settings {
    #[serde(default = "Settings::default_status_probe_addr")]
    status_probe_addr: SocketAddr,
    queue_url: String,
}

impl Settings {
    fn default_status_probe_addr() -> SocketAddr {
        "0.0.0.0:8080"
            .parse()
            .expect("default status probe address")
    }
}

fn version() -> String {
    format!(
        "{} {} ({}, {} build, {} [{}], {})",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        built_info::GIT_VERSION.unwrap_or("unknown"),
        built_info::PROFILE,
        built_info::CFG_OS,
        built_info::CFG_TARGET_ARCH,
        built_info::BUILT_TIME_UTC,
    )
}

#[tokio::main]
async fn run(settings: Settings) -> Result<(), Box<dyn std::error::Error>> {
    let token_file = var_os("AWS_WEB_IDENTITY_TOKEN_FILE");
    let role = var_os("AWS_ROLE_ARN");

    let aws_client =
        if token_file.map_or(true, |v| v.is_empty()) || role.map_or(true, |v| v.is_empty()) {
            AwsClient::shared()
        } else {
            debug!("using AWS Web Identity credential provider");
            let sqs_http = HttpClient::new()?;
            let cred_provider = AutoRefreshingProvider::new(WebIdentityProvider::from_k8s_env())?;
            AwsClient::new_with(cred_provider, sqs_http)
        };

    let client = SqsClient::new_with_client(aws_client, Region::default());

    let queue_url = settings.queue_url;
    let rcv = ReceiveMessageRequest {
        queue_url: queue_url.clone(),
        wait_time_seconds: Some(10),
        ..Default::default()
    };

    let queue = stream::repeat(rcv)
        .then(|rcv| {
            let client = client.clone();

            async move {
                client
                    .receive_message(rcv)
                    .await
                    .map_err(anyhow::Error::new)
            }
        })
        .and_then(|res| {
            debug!("{:?}", res);

            let client = client.clone();
            let queue_url = queue_url.clone();

            async move {
                if let Some(messages) = res.messages {
                    for msg in messages {
                        info!("{:?}", msg);

                        if let Some(receipt_handle) = msg.receipt_handle {
                            let del = DeleteMessageRequest {
                                queue_url: queue_url.clone(),
                                receipt_handle,
                            };

                            client
                                .delete_message(del)
                                .await
                                .map_err(anyhow::Error::new)?;
                        }
                    }
                }

                Ok(())
            }
        })
        .fuse();

    pin_mut!(queue);

    let mut status_probe = TcpListener::bind(&settings.status_probe_addr).await?;
    let probes = status_probe.incoming().fuse();
    pin_mut!(probes);

    let mut signals = SelectAll::new();
    signals.push(signal(SignalKind::interrupt()).expect("failed to register the interrupt signal"));
    signals.push(signal(SignalKind::quit()).expect("failed to register the quit signal"));
    signals.push(signal(SignalKind::terminate()).expect("failed to register the terminate signal"));
    // ignore SIGPIPE
    let _sigpipe = signal(SignalKind::pipe()).expect("failed to register the pipe signal");

    loop {
        select! {
            res = queue.try_next() => {
                if let Err(e) = res {
                    error!("Error processing queued messages: {:?}", e);
                    delay_for(Duration::from_secs(5)).await;
                }
            }

            _ = probes.next() => {
                debug!("status probe ping");
            }

            _ = signals.select_next_some() => {
                debug!("shutting down...");
                return Ok(());
            }
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    TracingSubscriber::builder()
        .with_env_filter(TracingEnvFilter::from_default_env())
        .json()
        .init();

    info!("{}", version());

    let mut settings = Config::new();
    settings.merge(Environment::new())?;

    run(settings.try_into()?)
}
