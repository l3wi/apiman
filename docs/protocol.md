# TrackMan interfaces

This is the reference for the two interfaces `trackman` uses:
- the TrackMan 4 local network API, for live shots;
- the TrackMan cloud GraphQL API, for historical sessions.

## Status

- **TM4 LAN API:** decoded from TrackMan's own device SDK. `trackman` was exercised against a mock TM4 that implements this document: discovery, description, REST, WebSocket subscription, keepalive, reconnect and SNTP. It has **not yet run against a real unit**.
- **Cloud API:** verified live on 2 October 2026. `trackman` signed in with the device-code login, listed activities, and pulled sessions with every stroke and measurement field.
- **Markers:** claims marked [INFERENCE] are not established from code or observation.

## Overview

| Path | Transport | Latency | Auth |
|---|---|---|---|
| **TM4 LAN API** | SSDP/mDNS discovery → HTTP REST + WebSocket on the unit | Real time, pushed per shot | None in viewer role |
| **Cloud GraphQL** | `POST https://api.trackmangolf.com/graphql` | After the session syncs | OIDC bearer token |
| Range bays | SignalR hub on a TrackMan Range site server | Real time | Range login and bay pairing (only at TrackMan Range facilities; not covered here) |

**There is no cloud real-time stream.** The GraphQL schema has no subscription root (`__schema { subscriptionType }` is `null`). Live data from a TM4 is only available on the local network.

## Provenance

- **Source app:** TrackMan Golf for Android, `dk.TrackMan.Range` 5.4.11 (version code 50411001).
- **How it was decoded:** the app is .NET for Android, so its assemblies were taken from `lib/arm64-v8a/libassembly-store.so` (an `XABA` v3 store with LZ4-compressed `XALZ` entries) and decompiled with ILSpy. The analysis was static; the app was not run.
- **Origin of the LAN protocol:** it comes from TrackMan's device SDK (`TrackMan.Api.Devices`, `TrackMan.Api.Data`), which the app bundles. The app does not drive a TM4 itself.
- **Cloud details:** these come from the app's GraphQL client (`TrackMan.Api.Gql`), its login code, and the public endpoints' OIDC discovery document and GraphQL introspection.

## 1. TM4 LAN API

### 1.1 Discovery

The SDK searches with both methods at once and deduplicates replies by USN:

- **SSDP:** multicast to `239.255.255.250:1900` with TTL 2, sent 3 times:
  ```
  M-SEARCH * HTTP/1.1
  HOST: 239.255.255.250:1900
  ST:urn:schemas-upnp-org:device:TrackMan:1
  MAN:"ssdp:discover"
  MX:3
  ```
  Take `LOCATION` from replies whose `ST` matches.
- **mDNS:** browse `_trackman._tcp`, using the TXT records `st=`, `location=` and `usn=`. `trackman` does not implement mDNS yet.
- **Direct:** an IP address maps to `http://<ip>:2869/`.

### 1.2 Device description

`GET <LOCATION>` returns UPnP XML in namespace `urn:schemas-upnp-org:device-1-0`, with `deviceType` set to `urn:schemas-upnp-org:device:TrackMan:1`. The elements used are:

| Element | Use |
|---|---|
| `api` | REST base URL |
| `cameraApi` | Camera REST base URL |
| `webSocket` | Event WebSocket URL; the port defaults to 80 |
| `presentationURL` | Web UI |
| `ntpServer` | Optional; falls back to the `presentationURL` host |
| `friendlyName`, `serialNumber`, `modelNumber` | Identity |

If the advertised URLs name a different host from the one contacted, the SDK rewrites them all to the contacted host.

`GET <api>` returns `{"Version": "x.y", "Date": ...}`. The minimum supported version is 2.0.

### 1.3 REST resources (relative to `<api>`)

Every resource is read with `GET`; writable resources take `POST` with a JSON body. JSON names are PascalCase unless marked *snake_case*. Null fields are omitted, and dates are ISO 8601 UTC.

| Route | R/W | Key fields |
|---|---|---|
| `systeminfo` | R | `DeviceName, Model, SwConfig, Type, SerialNumber, PartNumber, Features[], SystemState, SystemTime, Uptime, Errors[]` |
| `status` | R | `SystemState, TrackerState, Battery, GPS, Leveling` |
| `system/status` | R | `SystemState`, … |
| `setup` | R/W | `IsMeasuring, MeasurementMode, StartPosition{X,Y,Z}, ExpectedStartPosition, TargetPosition, LandingHeight, ExtrapolationHeight, Conditions{PlayerName, ClubType, ClubDimensions, BallType, Weather}, SaveDataFiles, ExportXmlFiles, FixedVerticalAngle?, FixedHorizontalAngle?` |
| `measure` | R/W | `IsMeasuring, MeasureTimeout` |
| `measure/frequencygroup/status`, `…/setup` | R, R/W | Radar frequency group |
| `sessionstatus` | R | Current operator: `Username, Operator, Software, IpAddress, LoginTime, LastSeenTime, ClientId` |
| `timesync/status` | R, *snake_case* | `io_ctrl{pulse_source, pulse_present, gps_present}, ntp{clock_offset, offset, server, frequency, stratum}, ptp{grandmaster_id, master_offset}, sync_source` |
| `IfNetwork/status` | R, *snake_case* | `ip`, … |
| `wireless/status`, `wireless/setup`, `ethernet/status`, `ethernet/setup` | R, R/W | Network configuration |
| `firmwarestatus`, `licensestatus` | R | — |
| `login` | POST | `{Username:"admin", Password:"", Operator, Software, ClientId, Force}` returns the token as a quoted string. It fails while another client holds the operator role, unless `Force` is set |
| `logout`, `validatesession` | POST | Header `Session-Id: <token>` |
| `systemtime?isotime=<ISO-8601 UTC>` | POST | Sets the TM4 clock |
| `reboot`, `Firmware`, `License` | POST | Device management |

**Roles:**
- A client without a token is a **viewer**, and viewers receive every event.
- Changing the unit requires the **operator** token. It is sent in the `Session-Id` header and bound to the WebSocket with `{"Type":"AuthToken","Payload":{"AuthToken":…}}`.
- `trackman` stays a viewer, so it never takes control from TPS or the TrackMan app.

### 1.4 WebSocket events

The envelope is `{"Type": str, "SubType": str?, "Id": str?, "Payload": any?}`.

```
→ {"Type":"Subscribe","Payload":{"MessageList":["Measurement","MeasurementDetail","LiveTrajectory","TrackerState","SystemState","Setup","SessionOpened","SessionClosed"]}}
← {"Type":"Acknowledge","SubType":"Subscribe"}
← {"Type":"Ping"}          → {"Type":"Pong"}
```

`"ALL"` subscribes to every topic: `SessionOpened, SessionClosed, Setup, FieldCalibration, SystemState, TrackerState, LiveTrajectory, Measurement, MeasurementDetail, SimulationState, SimulationTrajectory, Files, CameraEvent, Notify, Goodbye, Broadcast-Search, Broadcast-Notify, Broadcast-ByeBye, Broadcast-Event`.

| `Type` | Payload | Notes |
|---|---|---|
| `Measurement`, `LaunchData` | Golf `Measurement` (§1.5) | Each shot produces several messages that share the same `Id` |
| `LiveTrajectory` | `{"PositionList": LiveBallPosition \| LiveBallPosition[]}`, where `LiveBallPosition = {Time, Position[3]}` | In-flight ball positions, in m and s [INFERENCE] |
| `TrackerState` | `{"TrackerState": str}` | `Idle → ClubDetected → BallDetected → TrackConfirmed → PostProcessing → TrackComplete`; also `TrackLost`, `TrackAborted`, `Error` |
| `SystemState` | `{"SystemState": str}` | `Idle, Active, Measuring, MeasuringError, Error, …` |
| `MeasurementDetail` | `MeasurementDetails` | `ImpactLocation`, `OutsideTeeArea…`, `TrackerNoise{AcceptableLevelExceeded, MeasuredLevel[], NoisyChannels[]}`, `ExpiredLicense{InvalidMeasurements[]}` |
| `Setup` | `TrackManSetup` | Mode, tee and target positions |
| `SessionOpened`, `SessionClosed` | `SessionStatus` | Operator changes |
| `Files` / `MeasurementData` | `string[]` | Paths to raw measurement files |

The unit's `Ping` messages act as the keepalive.

### 1.5 Golf `Measurement`

All values are SI, according to the SDK's `[Unit]` attributes. Every field except `Id`, `Time` and `Kind` is optional.

- **Identity:** `Id` (a GUID shared by every message for one shot), `Time` (on the TM4 clock), `Kind`, and `ReducedAccuracy[]` (the names of parameters measured with degraded accuracy).
- **`Kind` progression:** `PreLaunchData` → `LaunchData` → `LiveApex` → `FlightData` → `Measurement`. Use the final `Measurement` for each `Id`; `LaunchData` arrives first. `Normalized` is the variant normalised to standard conditions, and `SpeedTraining` is a separate mode.
- **Geometry:** `TeePosition[3]` and `TargetPosition[3]` in m; `PlayerDexterity`, `DetectedClubCategory`.
- **Ball:** `BallSpeed` m/s; `LaunchAngle`, `LaunchDirection` deg; `SpinRate` rpm; `SpinAxis`, `GyroSpinAngle` deg; `SmashFactor`, `SmashIndex`, `SpinIndex`, `BallSpeedDifference`, `SpinRateDifference`.
- **Flight:** `Carry`, `Total`, `CarrySide`, `TotalSide`, `Side`, `Curve`, `MaxHeight`, `LandingHeight`, `LastData` in m; `LandingAngle` deg; `HangTime` s. `…Actual` variants hold the values for actual conditions.
- **Club:** `ClubSpeed` m/s; `AttackAngle`, `ClubPath`, `DynamicLoft`, `FaceAngle`, `SpinLoft`, `FaceToPath`, `SwingPlane`, `SwingDirection`, `DPlaneTilt`, `DynamicLie` deg; `SwingRadius`, `LowPointDistance`, `LowPointHeight`, `LowPointSide`, `ImpactOffset`, `ImpactHeight` m.
- **Roll and putting:** `SkidDistance`, `RollSpeed`, `RollPercentage`, `SpeedDrop`, `Break`, `TotalBreak`, `RollDeceleration`, `Bounces`, `EffectiveStimp`, `FlatStimp`, `Elevation`, `SlopePercentageSide`/`Rise`, `StrokeLength`, `BackswingTime`, `ForwardswingTime`, `Tempo`, `EntrySpeedDistance`.
- **Trajectories:** `BallTrajectory[]` and `ClubTrajectory[]`, each a `Trajectory{Kind, XFit[], YFit[], ZFit[], SpinRateFit[], TimeInterval[2], ValidTimeInterval[2], MeasuredTimeInterval[2]}`.
  - **Segment kinds:** ball segments are `Flight, Bounce, Roll, Skid, Slide`; club segments are `PreImpact, PostImpact`.
  - **Polynomials have ascending coefficients:** `p(t) = c[0] + c[1]·t + c[2]·t² + …`, with position in m and t in seconds within `TimeInterval`.
  - **Measured span:** `MeasuredTimeInterval` covers the radar-observed span; outside it the fit is extrapolated.
  - **Time origin and axes:** the time origin is assumed to be launch [INFERENCE]. The TrackMan axis convention is assumed to be X toward the target, Y up and Z right [INFERENCE].

### 1.6 Time

- **SNTP server:** the TM4 runs one on UDP 123 at the API host, and the SDK measures the TM4-to-host offset from it. `timesync/status` reports the unit's own NTP, PTP and GPS/1PPS state.
- **Converting shot times:** `Measurement.Time` is on the TM4 clock. Convert with `host_time = Time − offset`.
- **Unknown semantics:** whether `Time` marks impact, track start or message creation is not established.

## 2. Cloud GraphQL

### 2.1 Endpoint

- **URL:** `POST https://api.trackmangolf.com/graphql` with `Authorization: Bearer <access_token>`.
- **Schema:** introspection is open without authentication (about 2,200 types).
- **No real time:** there are no subscriptions.

### 2.2 Login

- **Authority:** `https://login.trackmangolf.com`. `/.well-known/openid-configuration` lists the endpoints and grants, including `urn:ietf:params:oauth:grant-type:device_code`.
- **App client:** the TrackMan Golf app uses a public client with no secret, `client_id = old-golf-app.c686e909-5102-45ac-9860-8d0b789073ae`. It requests the scopes `openid offline_access profile https://auth.trackman.com/dr/simulate https://auth.trackman.com/dr/cloud https://auth.trackman.com/login/autoconsent`, and refresh tokens are issued. The app itself signs in with the authorization code flow and PKCE, using the custom-scheme redirect `dk.trackman.range://oauth`.
- **Device code flow:** the client also accepts the device authorization grant, which suits a CLI:
  1. `POST /connect/deviceauthorization` (`client_id`, `scope`) returns `user_code` and `verification_uri_complete` (`https://login.trackmangolf.com/device?userCode=…`).
  2. Poll `POST /connect/token` with `grant_type=urn:ietf:params:oauth:grant-type:device_code`, `client_id` and `device_code`, honouring `authorization_pending` and `slow_down`.
  3. Refresh with `grant_type=refresh_token`.

### 2.3 Activities

`me.activities(kinds, timeFrom, timeTo, skip, take, includeHidden)` uses offset paging (`items`, `pageInfo.hasNextPage`, `totalCount`). `ActivityKind` wire values are SCREAMING_SNAKE_CASE.

Activities that carry strokes implement `SessionActivityInterface { strokes: [Stroke!] }`:

| GraphQL type | `ActivityKind` |
|---|---|
| `SessionActivity` | `SESSION` |
| `ShotAnalysisSessionActivity` | `SHOT_ANALYSIS` |
| `MapMyBagSessionActivity` | `MAP_MY_BAG` |
| `VirtualRangeSessionActivity` | `VIRTUAL_RANGE` |
| `PerformancePuttingSessionActivity` | `PERFORMANCE_PUTTING` |
| `SimulatorSessionActivity` | `SIMULATOR` |
| `TracySessionActivity` | `TRACY` |
| `TestActivity` | `TEST` |
| `CombineTestActivity` | `COMBINE_TEST` |

Range-bay activities, such as `RANGE_PRACTICE`, carry `RangeStroke`, which is a different type.

### 2.4 Strokes

- **Fetching:** fetch an activity with `node(id)` and an inline fragment on `SessionActivityInterface`.
- **`Stroke` fields:** `time, club, ball, targetDistance, tags, clubData, impactLocation, measurementDetails, measurement, normalizedMeasurement`.
- **Club names:** values are codes such as `Driver`, `6Iron`, `56Wedge` and `PitchingWedge`.
- **Measurement fields:** the cloud `Measurement` has the same 72 scalar fields as §1.5, in camelCase, plus `ballTrajectory` and `clubTrajectory`. Observed values are consistent with SI; for example, `ballSpeed` 50.3 for a topped driver is m/s.
- **trackman's queries:** they are in [`src/graphql/`](../src/graphql). They were generated from the introspection schema and validated against the server.

### 2.5 History

The TM4 LAN API has no session or history query. Synced sessions in the cloud are the only after-the-fact source.
