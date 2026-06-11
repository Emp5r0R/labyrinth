# Architecture Guide

Labyrinth is organized around clear runtime roles and manager boundaries. Keep
new work aligned with these boundaries so the project remains easy to extend.

## Runtime Roles

Server:

- Owns operator state, authentication policy, certificate generation, connected
  agent registry, dweller registry, topology snapshots, smart access planning,
  active Ariadne tunnels, Portal listeners, and the browser dashboard.
- Runs either interactive mode or headless mode.
- Defaults to TCP/TLS and can run QUIC/UDP when selected with
  `--transport quic`.

Agent:

- Connects outbound to the server.
- Registers host profile, operating system, interface data, and low-noise
  outbound reachability state.
- Executes command, shell, upload, download, Portal, and Ariadne requests from
  the server.

Dweller:

- Runs as a persistent remembered endpoint.
- Can listen for inbound server connections.
- Can check in to callback servers when reachable.
- Supports hibernating task polling with sleep, jitter, and bounded task
  batches.
- Stores and reports operator context such as observed parent path, callback
  targets, and reachability.

## Module Map

CLI and shared:

- `src/main.rs`: compatibility wrapper entry point.
- `src/bin/`: dedicated binary entry points.
- `src/cli.rs`: Clap argument definitions and role dispatch.
- `src/protocol.rs`: typed protocol messages shared by server, agent, and
  dweller.
- `src/transport.rs`: transport mode enum and CLI value handling.
- `src/security.rs`: certificate and fingerprint helpers.
- `src/config.rs`, `src/error.rs`, `src/styling.rs`: shared support code.

Server:

- `src/server/mod.rs`: server startup, listener setup, interactive command
  dispatch, shell workflows, and headless operation.
- `src/server/core.rs`: central `LabyrinthServer` state.
- `src/server/agent_manager.rs`: connected agent registry.
- `src/server/agent_connection.rs`: per-agent message routing.
- `src/server/chain_manager.rs`: smart multi-hop planning and access
  orchestration.
- `src/server/topology.rs`: route inference, ownership, shared networks, and
  conflict detection.
- `src/server/network_map.rs`: terminal map rendering.
- `src/server/dashboard.rs`: read-only browser visualization and JSON snapshot
  API.
- `src/server/tunnel_manager.rs`: Ariadne tunnel lifecycle.
- `src/server/reverse_port_forward.rs`: Portal listener management.
- `src/server/quic_stream_bridge.rs`: native QUIC stream bridge for proxied
  flows.
- `src/server/dweller_manager.rs`: dweller drop, connect, configure, and task
  workflows.
- `src/server/dweller_registry.rs`: persisted dweller records and task queue.
- `src/server/certificate.rs`, `src/server/privileges.rs`, `src/server/ui.rs`:
  certificate, privilege, and display helpers.
- `src/server/netstack_bridge_windows.rs`: Windows netstack bridge boundary.

Agent:

- `src/agent/core.rs`: agent and dweller runtime loops, registration,
  reachability, callback, hibernation, and task handling.
- `src/agent/connection.rs`: TCP/TLS, QUIC/UDP, SOCKS, retry, and connection
  setup.
- `src/agent/tls_config.rs`: TLS and certificate verification.
- `src/agent/command_executor.rs`: OS-aware command execution workflows.
- `src/agent/pty_shell.rs`: interactive PTY shell support.
- `src/agent/reverse_port_forward.rs`: agent-side Portal handling.
- `src/agent/streaming_manager.rs`: agent-side streaming handlers.
- `src/agent/system_info.rs`: host and interface collection.

Streaming:

- `src/streaming/traits.rs`: abstractions used by stream managers.
- `src/streaming/models.rs`: stream, connection, and metric models.
- `src/streaming/connection_manager.rs`: connection lifecycle.
- `src/streaming/stream_manager.rs`: bidirectional stream handling.
- `src/streaming/recovery.rs`: cleanup and recovery coordination.
- `src/streaming/metrics.rs`: metrics and health reporting.
- `src/streaming/resource_manager.rs` and `src/streaming/test_interfaces.rs`:
  test support.

## SOLID Boundaries

Single responsibility:

- Protocol definitions belong in `protocol.rs`.
- CLI shape belongs in `cli.rs`.
- Server state belongs in `server/core.rs`.
- Topology calculations belong in `server/topology.rs`.
- Smart path planning belongs in `server/chain_manager.rs`.
- Dweller persistence belongs in `server/dweller_registry.rs`.

Open and closed:

- Add transports through `TransportMode` and transport-specific modules.
- Add dashboard fields by extending typed snapshots and keeping terminal map
  snapshots in sync.
- Add dweller tasks through typed task models instead of ad hoc strings.

Interface segregation:

- Keep streaming traits focused on stream lifecycle and payload handling.
- Keep dashboard endpoints read-only unless a dedicated control API with auth is
  intentionally designed.

Dependency inversion:

- Route high-level workflows through manager APIs instead of reaching into
  another module's internal state.
- Prefer typed protocol messages over string commands between server and agent.

## Transport Model

TCP/TLS is the default transport. It is compatible with SOCKS5 proxy mode and
uses framed protocol messages on a TLS connection.

QUIC/UDP is optional. It keeps certificate trust semantics but uses QUIC for
the server-agent connection. For supported Portal and Ariadne flows, each
internal TCP connection opens a separate QUIC bidirectional stream. This reduces
head-of-line blocking and avoids the TCP-over-TCP feedback loop.

HTTP, HTTPS, and DNS dweller callback transports are currently accepted as
configuration and planning labels. They need dedicated listener implementations
before they can carry task traffic.

## Topology and Smart Access

Topology is derived from:

- Connected agent interface data.
- Connected and remembered dweller records.
- Active Ariadne tunnels.
- Active Portal forwards.
- Route ownership and shared CIDR calculations.
- Low-noise outbound reachability checks.
- Stored dweller parent path metadata.

`plan` must remain read-only. `access` must show the selected route and require
confirmation before changing state. The path application should be idempotent:
reuse active tunnels, avoid duplicate listeners, connect reachable dwellers
only when useful, and refresh topology after state changes.

Stored dweller paths are operator context. They must not override live route
ownership, active tunnel state, or current reachability.

## Visualization Model

Terminal map and browser dashboard should derive from the same server snapshot
concepts:

- Nodes: server, agents, dwellers, networks, and forwards.
- Edges: encrypted transport, local or unencrypted connection, tunnel, route,
  and port-forward relationships.
- Status: online/offline, selected target, reachability, conflicts, active
  tunnels, hibernation, task state, and callback configuration.

The dashboard is read-only in this version. Keep it bound to localhost by
default. Add mutation endpoints only with explicit authentication and a stable
terminal-first workflow to mirror.

## Cross-Platform Notes

- Linux supports TUN-backed Ariadne on the server when privileges are available.
- Windows agent and dweller behavior should be kept functional for command,
  shell, file transfer, callback, hibernation, and Portal workflows.
- Platform-specific code should be isolated behind target-specific modules and
  compile gates.
