use crate::common::get_arg_or;
use hbb_common::{anyhow::Context, log, ResultType};
use maxminddb::{geoip2, Reader};
use std::{collections::HashMap, net::IpAddr};

type Coordinates = [f64; 2];

pub(crate) struct GeoRelay {
    database: Reader<Vec<u8>>,
    locations: HashMap<String, Coordinates>,
}

impl GeoRelay {
    pub(crate) fn from_config() -> ResultType<Option<Self>> {
        let database = get_arg_or("geoip-db", std::env::var("GEOIP_DB").unwrap_or_default());
        let locations = get_arg_or(
            "relay-locations",
            std::env::var("RELAY_LOCATIONS").unwrap_or_default(),
        );
        if database.is_empty() && locations.is_empty() {
            return Ok(None);
        }
        let routing = Self::load(&database, &locations)?;
        log::info!(
            "Geographic relay routing enabled ({} locations)",
            routing.locations.len()
        );
        Ok(Some(routing))
    }

    fn load(database: &str, locations: &str) -> ResultType<Self> {
        let database =
            Reader::open_readfile(database).context("Cannot open GeoIP City database")?;
        hbb_common::anyhow::ensure!(
            database.metadata.database_type.ends_with("-City"),
            "GeoIP database must be a GeoIP2 or GeoLite2 City database"
        );
        let locations =
            std::fs::read_to_string(locations).context("Cannot read relay locations")?;
        let locations: HashMap<String, Coordinates> =
            serde_json::from_str(&locations).context("Invalid relay locations JSON")?;
        for (host, coordinates) in &locations {
            hbb_common::anyhow::ensure!(
                valid_coordinates(*coordinates),
                "Invalid latitude/longitude for relay {host}"
            );
        }
        hbb_common::anyhow::ensure!(!locations.is_empty(), "Relay locations must not be empty");
        Ok(Self {
            database,
            locations,
        })
    }

    pub(crate) fn validate_relays(&self, relays: &[String]) -> ResultType<()> {
        for host in relays {
            hbb_common::anyhow::ensure!(
                self.locations.contains_key(host),
                "Missing geographic location for relay {host}"
            );
        }
        Ok(())
    }

    fn locate(&self, ip: IpAddr) -> Option<Coordinates> {
        let ip = match ip {
            IpAddr::V6(ip) => ip
                .to_ipv4_mapped()
                .map(IpAddr::V4)
                .unwrap_or(IpAddr::V6(ip)),
            ip => ip,
        };
        let city: geoip2::City = self.database.lookup(ip).ok()?;
        let location = city.location?;
        let coordinates = [location.latitude?, location.longitude?];
        valid_coordinates(coordinates).then_some(coordinates)
    }

    pub(crate) fn select<'a>(&self, online: &'a [String], a: IpAddr, b: IpAddr) -> Option<&'a str> {
        nearest(online, &self.locations, self.locate(a), self.locate(b))
    }
}

fn valid_coordinates([latitude, longitude]: Coordinates) -> bool {
    latitude.is_finite()
        && longitude.is_finite()
        && (-90.0..=90.0).contains(&latitude)
        && (-180.0..=180.0).contains(&longitude)
}

fn distance(a: Coordinates, b: Coordinates) -> f64 {
    let latitude = ((b[0] - a[0]).to_radians() / 2.0).sin().powi(2);
    let longitude = ((b[1] - a[1]).to_radians() / 2.0).sin().powi(2);
    let haversine = latitude + a[0].to_radians().cos() * b[0].to_radians().cos() * longitude;
    2.0 * haversine.clamp(0.0, 1.0).sqrt().asin()
}

fn nearest<'a>(
    online: &'a [String],
    locations: &HashMap<String, Coordinates>,
    a: Option<Coordinates>,
    b: Option<Coordinates>,
) -> Option<&'a str> {
    if a.is_none() && b.is_none() {
        return None;
    }
    online
        .iter()
        .filter_map(|host| {
            let location = *locations.get(host)?;
            let da = a.map(|a| distance(a, location)).unwrap_or(0.0);
            let db = b.map(|b| distance(b, location)).unwrap_or(0.0);
            Some((host.as_str(), da + db, da.max(db)))
        })
        .min_by(|a, b| {
            // Microradian rounding avoids unstable ties along the same great circle.
            let total_a = (a.1 * 1_000_000.0).round();
            let total_b = (b.1 * 1_000_000.0).round();
            total_a
                .total_cmp(&total_b)
                .then_with(|| a.2.total_cmp(&b.2))
        })
        .map(|(host, _, _)| host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_uses_both_peers_and_only_online_relays() {
        let locations = HashMap::from([
            ("west".to_owned(), [0.0, -60.0]),
            ("middle".to_owned(), [0.0, 0.0]),
            ("east".to_owned(), [0.0, 60.0]),
        ]);
        let all = vec!["west".into(), "middle".into(), "east".into()];
        let a = Some([0.0, -60.0]);
        let b = Some([0.0, 60.0]);
        assert_eq!(nearest(&all, &locations, a, b), Some("middle"));
        assert_eq!(nearest(&all, &locations, b, a), Some("middle"));
        assert_eq!(nearest(&all, &locations, a, a), Some("west"));
        assert_eq!(nearest(&all, &locations, None, b), Some("east"));
        assert_eq!(nearest(&all, &locations, None, None), None);
        assert_eq!(nearest(&all[2..], &locations, a, a), Some("east"));
        assert_eq!(nearest(&[], &locations, a, b), None);
        assert!(valid_coordinates([90.0, -180.0]));
        assert!(!valid_coordinates([91.0, 0.0]));
        assert!(!valid_coordinates([0.0, f64::NAN]));
        assert!((distance([0.0, 179.0], [0.0, -179.0]).to_degrees() - 2.0).abs() < 1e-9);
        assert!(distance([90.0, 0.0], [-90.0, 180.0]).is_finite());
    }

    #[test]
    fn city_database_lookup_supports_ipv4_ipv6_and_unknown_ips() {
        let database =
            Reader::from_source(include_bytes!("../tests/data/GeoIP2-City-Test.mmdb").to_vec())
                .unwrap();
        let routing = GeoRelay {
            database,
            locations: HashMap::from([("london".into(), [51.5, -0.1])]),
        };
        let ip = "81.2.69.160".parse().unwrap();
        let position = routing.locate(ip).unwrap();
        assert!((position[0] - 51.5142).abs() < 0.001);
        assert_eq!(
            routing.locate("::ffff:81.2.69.160".parse().unwrap()),
            Some(position)
        );
        assert!(routing.locate("2001:218::".parse().unwrap()).is_some());
        assert!(routing.locate("127.0.0.1".parse().unwrap()).is_none());
        assert_eq!(routing.select(&["london".into()], ip, ip), Some("london"));
        assert!(routing.validate_relays(&["unknown".into()]).is_err());
    }
}
