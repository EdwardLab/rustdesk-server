# Geographic relay routing

This fork adds geographic relay selection to the `forapi` branch of
`lejianwen/rustdesk-server`. Existing RustDesk clients and the API service
remain compatible. Only `hbbs` needs the routing change; `hbbr` can remain
an ordinary OSS relay.

## Selection

For each new connection, `hbbs` looks up both peers' public IP addresses in
a local GeoIP City database. It selects the online relay with the lowest
sum of great-circle distances to the two peers. Essentially equal totals
are resolved by minimizing the longer of the two legs, then configured
relay order. Distance is a geographic estimate, not a latency measurement.
VPNs, mobile carriers and stale GeoIP records can affect the result.

If only one peer has a known location, selection uses that peer. If neither
has a location, the server uses the existing round-robin policy. Without
GeoIP configuration the server retains round-robin routing. Direct peer
connections remain enabled and do not use the relay unless needed.

The existing TCP health probe runs every three seconds with a three-second
timeout, including when there is only one relay. Unchecked nodes are not
eligible until their first successful probe. Failed nodes are removed even
when all nodes fail; recovered nodes rejoin. The probe checks TCP
reachability, not bandwidth or the completion of a remote desktop session.
Existing sessions are not moved between relays.

## Configuration

1. Deploy `hbbr` in each region with the same authentication public key as
   the main server, and allow its TCP port (normally 21117, and 21119 for
   WebSocket clients). Do not publish the private signing key.
2. Use the free [MaxMind GeoLite2 City database](https://dev.maxmind.com/geoip/geolite2-free-geolocation-data/).
   Create a free MaxMind account and download `GeoLite2-City.mmdb` in MMDB
   format. Country and ASN databases do not provide the required coordinates.
   Follow the database license and update requirements. The repository's
   test fixture is synthetic and unsuitable for deployment.
3. Create `relay-locations.json`, specifying each relay's actual data-center
   coordinates as `[latitude, longitude]`. Names must exactly match the
   entries passed to `hbbs -r`, including ports. For example:

   ```json
   {
     "sg.example.net:21117": [1.3521, 103.8198],
     "us.example.net:21117": [37.3382, -121.8863],
     "eu.example.net:21117": [50.1109, 8.6821]
   }
   ```

4. Run the fork's `hbbs` with both environment variables:

   ```sh
   GEOIP_DB=/data/GeoLite2-City.mmdb \
   RELAY_LOCATIONS=/data/relay-locations.json \
   ./hbbs -k _ -r sg.example.net:21117,us.example.net:21117,eu.example.net:21117
   ```

   The paths can also be set through the existing `.env` or `-c` INI file:

   ```ini
   geoip-db=/data/GeoLite2-City.mmdb
   relay-locations=/data/relay-locations.json
   ```

   Configure both paths or neither. Unreadable files, the wrong database type,
   invalid coordinates, and missing relay coordinates fail startup rather
   than silently claiming geographic routing is enabled. Explicit coordinate
   configuration avoids blocking DNS or external HTTP lookups during selection.
   Docker mounts should make the two files readable inside `hbbs`.

5. Keep the client ID server and Key settings. **Clear the client's Relay
   Server field**, including any account/API-provided fixed relay setting,
   so it accepts the relay selected by `hbbs`. Changing settings does not
   terminate or reroute already-established sessions.

## Inspect and reload

The existing loopback-only `hbbs` console is on the NAT-test port
(`hbbs port - 1`, normally 21115). Send a command using `nc`:

```sh
printf 'test-geo 8.8.8.8 1.1.1.1' | nc 127.0.0.1 21115
printf 'reload-geo' | nc 127.0.0.1 21115
```

`test-geo` reports the selector's current relay, including round-robin
fallback. Use actual endpoint IPs when diagnosing a connection.
`reload-geo` reloads the database and coordinates from the configured paths;
check the `hbbs` logs for success or failure. If validation fails, the
previous working routing data remains active. Update coordinates before
adding a new relay through the existing `relay-servers` console command.

## Build and checks

```sh
git submodule update --init --recursive
cargo build --locked --bins
cargo test --locked --lib
```

See `tests/geo-relay-smoke.py` for a local multi-process routing and health
check. It uses synthetic test data and temporary relay ports. It does not
change an installed RustDesk client or a deployed server.

## Keep up with upstream

The fork preserves the original `forapi` branch. `geo-relay` contains the
feature. The geographic code lives in `src/geo_relay.rs`, with integration
in the existing central selector. Relay health fixes are a separate commit.
There are no protocol changes, client forks, API forks, or submodule patches.

Merge upstream into the feature branch and rerun checks:

```sh
git remote add upstream https://github.com/lejianwen/rustdesk-server.git
git fetch upstream
git switch geo-relay
git merge upstream/forapi
git submodule update --init --recursive
cargo test --locked --lib
cargo build --locked --bins
python3 tests/geo-relay-smoke.py
```

If upstream changes dependency manifests, regenerate and review `Cargo.lock`
with `cargo check`, then rerun the locked checks. Preserve upstream submodule
updates. Resolve central-selector conflicts by keeping upstream connection
handling and the geographic selector hook. CI verifies feature-branch builds;
it does not merge upstream automatically.
