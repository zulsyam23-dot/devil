# The devil registry protocol

Version 3 (`devil-lock-v3`).

A registry is any HTTP server that can answer five requests. There is no query
language, no database schema and no required server implementation: every
response is a static document that can be cached at the edge.

---

## Endpoints

```text
GET  {base}/index/{name}          -> PackageIndex       (application/json)
GET  {base}/dl/{name}/{version}  -> .devil archive     (application/gzip)
GET  {base}/api/v1/search?q=&limit=
                                 -> SearchResponse     (application/json)
POST {base}/api/v1/publish       -> PublishResponse    (multipart/form-data)
POST {base}/api/v1/yank          -> 200                (application/json)
```

`{base}` never has a trailing slash. A registry that receives an unknown path
answers `404`.

Authenticated requests carry `Authorization: Bearer <token>`. The client sends
it on every request; servers may ignore it for read-only endpoints.

---

## `GET /index/{name}`

Returns every published version of one package. A package that does not exist
returns `404`, which the client treats as an empty index so it can produce a
useful "no matching version" message.

```json
{
  "name": "greeter",
  "updated-at": "2026-03-01T10:00:00Z",
  "versions": {
    "0.1.0": {
      "name": "greeter",
      "version": "0.1.0",
      "cksum": "sha256-5918e0b6bbeab6b533add7db9d724d47a9fd2fd4208bf76c2c5b4d009be9173e",
      "deps": [
        {
          "name": "tiny-log",
          "req": "^0.3",
          "default-features": true,
          "features": ["ansi"],
          "kind": "normal",
          "optional": false
        }
      ],
      "features": { "ansi": [], "default": ["ansi"] },
      "yanked": false,
      "size": 1028,
      "published-at": "2026-03-01T10:00:00Z",
      "downloads": 412,
      "edition": "es2022",
      "description": "Friendly greetings.",
      "license": "MIT",
      "repository": null,
      "keywords": ["greeting"]
    }
  }
}
```

### Field notes

| Field        | Meaning                                                                  |
| ------------ | ------------------------------------------------------------------------ |
| `cksum`      | `sha256-<hex>` over the **archive bytes**, not the extracted tree.         |
| `deps[].req` | A semver requirement, e.g. `^0.3`, `=1.2.3`, `*`.                        |
| `deps[].kind` | `normal`, `dev` or `peer`. `dev` entries are never resolved.              |
| `deps[].optional` | Pulled in only when a feature of the same name is enabled.            |
| `yanked`     | Still installable by existing lockfiles; never selected for fresh ones.   |
| `size`       | Archive size in bytes. Informational.                                     |

The index must contain the **full dependency list** for each version, including
optional ones. Resolution never downloads a package just to read its
dependencies.

---

## `GET /dl/{name}/{version}`

Returns the raw `.devil` archive. The client verifies `cksum` before extracting
and refuses to continue on a mismatch.

The response body is exactly what was uploaded at publish time — registries are
expected to store the file verbatim, which is what makes the checksum stable.

---

## `POST /api/v1/publish`

`multipart/form-data` with two parts:

- `metadata` — the JSON `PublishRequest`
- `package` — the `.devil` archive

```json
{
  "name": "greeter",
  "version": "0.1.0",
  "cksum": "sha256-5918…",
  "size": 1028,
  "description": "Friendly greetings.",
  "readme": "# greeter\n…",
  "deps": [ { "name": "tiny-log", "req": "^0.3" } ],
  "yanked": false
}
```

Response:

```json
{ "ok": true, "warnings": [], "message": null }
```

A server should return:

| Status | Meaning                                                          |
| ------ | ---------------------------------------------------------------- |
| `200`  | Published.                                                        |
| `401`  | Missing or invalid token.                                         |
| `403`  | Token is valid but may not publish this name.                     |
| `409`  | Version already exists and is not yankable.                       |

Publishing an immutable version twice must fail. To withdraw a broken release,
yank it instead — lockfiles that already reference it keep working.

---

## `POST /api/v1/yank`

```json
{ "name": "greeter", "version": "0.1.0", "yanked": true, "reason": "regressions" }
```

Yanking never removes bytes; it only removes the version from the set a fresh
resolve will consider.

---

## `GET /api/v1/search?q=&limit=`

```json
{
  "total": 1,
  "results": [
    {
      "name": "greeter",
      "version": "0.1.0",
      "description": "Friendly greetings.",
      "downloads": 412
    }
  ]
}
```

A registry that does not implement search may return an empty result set.

---

## Minimal static layout

Because every read endpoint is a static document, a registry can be a plain
directory served by any web server:

```text
registry/
├── index/
│   └── greeter.json          # the PackageIndex document
└── dl/
    └── greeter/
        └── 0.1.0             # the .devil archive bytes
```

`devil registry index <archives> --dir ./registry` builds this layout from one
or more `.devil` files, and `devil registry serve` exposes it over HTTP.

---

## Versioning rules

- Package names are lowercase ASCII letters, digits, `-` and `_`, starting with
  a letter.
- Versions follow semver.
- A published version is immutable.
- `devil.lock` records the archive checksum, so a registry that mutates an
  existing archive is detected immediately as a checksum mismatch.