# API

The public surface of `commy_sdk_rust`, taken from the doc comments in `src/`. `cargo doc --open` has the full signatures.
Example: [README.md](README.md). How the pieces work together: [ARCHITECTURE.md](ARCHITECTURE.md).

## Client

A `Client` is one connection. It does what needs no login; everything else is on the `Session` that `authenticate` returns.

| method | what it does |
|---|---|
| `Client::new(url)`, `with_id(url, id)` | create a client (no connection yet) |
| `with_tls_root_pem(pem)` | trust exactly these certificates instead of the system roots (a server with a self-signed certificate) |
| `with_dev_scopes(&[..])` | development servers only: authenticate with exactly these scopes instead of administrator |
| `connect()` / `disconnect()` | open and close the WSS connection |
| `authenticate(tenant, credentials)` | authenticate to a tenant and return its `Session` |
| `initialize(url, tenant, credentials)` | `new` + `connect` + `authenticate` in one call; returns the `Session` |
| `heartbeat()` | send a heartbeat; waits up to 10 s for the answer but does not fail without one |
| `id()`, `server_url()`, `connection_state()`, `is_connected()` | read the connection |
| `authenticated_tenants()`, `is_authenticated_to(tenant)`, `idle_seconds()` | read the authentication state |

Credentials: `auth::api_key(key)` and `auth::jwt(token)` are what the Commy server accepts; `auth::basic(user, password)` exists on the
client but the server refuses it.

## Session

A tenant-bound handle; `Session` is `Clone`. `session.client()`, `tenant_id()` and `auth_context()` read it.

**Services**

| method | what it does |
|---|---|
| `create_service(name, size)` | create a service of `size` bytes; returns its id |
| `get_service(name)` | an existing service (read-only, no side effects) |
| `delete_service(name)` | delete a service |
| `create_tenant(id, name)`, `delete_tenant(id)` | tenant administration (needs the rights) |

**Access** (`grant` / `revoke` need admin rights on the tenant or service)

| method | what it does |
|---|---|
| `grant(client_id, service_id: Option, permissions: &[&str])` | give permissions: `tenant:read\|write\|admin`, `service:read\|write\|admin\|resize`; tenant-wide, or on one service |
| `revoke(client_id, service_id: Option, permissions: &[&str])` | take permissions away; with a `service_id` and no permissions the client is removed from the service (it gets `Loss{REVOKED}` and the file rotates) |
| `list_grants(client_id: Option, service_id: Option)` | list grants, optionally of one client and/or one service |

**Shared bytes**

| method | what it does |
|---|---|
| `set_local_mapping(service_id, enabled)` | open or close a service to local mapping (closed by default; needs `service:admin`) |
| `negotiate_local(service_id)` | run the local-mapping handshake and return the result |
| `open_shared(service_id)` | map locally when offered and start tracking in scan mode; `None` means the service stays on WSS ranges |
| `open_remote(service_id)` | fetch the whole file and keep a private copy current |
| `shared_write(service_id, offset, bytes)` | write; nothing is sent until `flush_shared` |
| `shared_read(service_id, offset, len)` | copy bytes out |
| `flush_shared(service_id)` | send the changed ranges; returns the highest `seq` seen |
| `poll_shared_updates(wait)` | wait for one server message and apply it if it is a range update; `true` if one was applied |
| `poll_shared_events(wait)` | the same, and also reports rotation and loss events (`SharedEvent`) |
| `unsafe arm_shared_trap(service_id, pool_pages)` | opt in to write trapping instead of scanning; read the safety contract in [ARCHITECTURE.md](ARCHITECTURE.md) |

**Rotation** (resize, revoke): `request_rotation`, `drain_for_rotation`, `begin_fill`, `finish_fill`, `abort_fill`,
`complete_rotation`. The sequence is in [ARCHITECTURE.md](ARCHITECTURE.md).

## Errors

Every call returns `commy_sdk_rust::Result<T>`; the error is `CommyError`. The ones a caller usually tells apart:

| variant | when |
|---|---|
| `AuthenticationFailed`, `Unauthorized` | the credentials were refused, or the request needs a login |
| `PermissionDenied` | logged in, but missing a permission for this operation |
| `NotFound`, `ServiceNotFound`, `TenantNotFound` | the named thing does not exist |
| `AlreadyExists` | `create_service` for a name that is taken |
| `ConnectionLost`, `Timeout` | the connection dropped, or no answer in time |
| `InvalidState`, `InvalidOffset` | a rotation is draining; or a read or write outside the file |

## Permissions

The server decides. A permission is a string (`tenant:*`, `service:*`); `session.auth_context().has_permission("service:write")`
reads what was granted at authentication. The list of operations each permission allows is the Commy server's documentation.
