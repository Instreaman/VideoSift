use std::net::{Ipv4Addr, SocketAddr};
use std::time::Duration;

use reqwest::{Client, Response, Url, redirect::Policy};
use tokio::net::lookup_host;
use tokio::sync::Mutex;
use tokio::time::Instant;

use super::request::{DEADLINE, RequestError, destination_allowed};

static LAST_START: Mutex<Option<Instant>> = Mutex::const_new(None);

async fn claim_request_slot(last: &Mutex<Option<Instant>>) -> Result<(), RequestError> {
    let mut previous = last.lock().await;
    let now = Instant::now();
    if let Some(start) = *previous {
        let remaining = Duration::from_secs(1).saturating_sub(now - start);
        if !remaining.is_zero() {
            return Err(RequestError::RateLimited {
                retry_after: Some(remaining),
            });
        }
    }
    *previous = Some(now);
    Ok(())
}

pub(super) async fn get(url: Url) -> Result<Response, RequestError> {
    let host = url
        .host_str()
        .filter(|_| destination_allowed(&url))
        .ok_or(RequestError::Destination)?;
    claim_request_slot(&LAST_START).await?;
    let resolved = lookup_host((host, 443))
        .await
        .map_err(|_| RequestError::Transport)?;
    let addresses = destination_addresses(resolved)?;
    let client = Client::builder()
        .https_only(true)
        .no_proxy()
        .redirect(Policy::none())
        .retry(reqwest::retry::never())
        .resolve_to_addrs(host, &addresses)
        .timeout(DEADLINE)
        .build()
        .map_err(|_| RequestError::Transport)?;
    client.get(url).send().await.map_err(RequestError::from)
}

/// Pin exactly the checked IPv4 addresses; never delegate a second lookup to the client.
fn destination_addresses(
    resolved: impl Iterator<Item = SocketAddr>,
) -> Result<Vec<SocketAddr>, RequestError> {
    let mut allowed = Vec::new();
    for addr in resolved {
        if let SocketAddr::V4(v4) = addr {
            if !public_ipv4(*v4.ip()) {
                return Err(RequestError::Destination);
            }
            allowed.push(addr);
        }
    }
    if allowed.is_empty() {
        return Err(RequestError::Destination);
    }
    Ok(allowed)
}

fn public_ipv4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !ip.is_private()
        && !ip.is_loopback()
        && !ip.is_link_local()
        && !ip.is_documentation()
        && !matches!(
            (a, b, c),
            (0, _, _)
                | (100, 64..=127, _)
                | (192, 0, 0)
                | (192, 88, 99)
                | (198, 18..=19, _)
                | (224..=255, _, _)
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn spaces_live_request_starts_before_dns() {
        let last = Mutex::new(None);
        claim_request_slot(&last).await.unwrap();
        assert_eq!(
            claim_request_slot(&last).await,
            Err(RequestError::RateLimited {
                retry_after: Some(Duration::from_secs(1))
            })
        );
        tokio::time::advance(Duration::from_secs(1)).await;
        claim_request_slot(&last).await.unwrap();
    }

    #[test]
    fn pins_public_ipv4_and_rejects_mixed_or_unusable_answers() {
        let public: SocketAddr = "8.8.8.8:443".parse().unwrap();
        for host in [
            "0.1.2.3",
            "127.1.2.3",
            "10.0.0.1",
            "100.64.1.1",
            "172.31.0.1",
            "169.254.169.254",
            "192.0.0.1",
            "192.0.2.1",
            "192.168.1.1",
            "198.19.0.1",
            "224.0.0.1",
            "255.255.255.255",
        ] {
            let denied = SocketAddr::new(host.parse().unwrap(), 443);
            assert_eq!(
                destination_addresses([public, denied].into_iter()),
                Err(RequestError::Destination)
            );
        }
        let v6 = "[::1]:443".parse().unwrap();
        assert_eq!(
            destination_addresses([v6, public].into_iter()).unwrap(),
            [public]
        );
        assert_eq!(
            destination_addresses([v6].into_iter()),
            Err(RequestError::Destination)
        );
    }
}
