# Implementation Plan — Persistence & Infrastructure Capabilities

สถานะ: **plan เท่านั้น ยังไม่ implement**
ขอบเขต: ให้ผู้ใช้เลือก ORM / relational DB / document-analytics / cache / time-series / object storage
ได้ โดย **ไม่แตก template ใหม่ต่อ combination**

---

## 0. สิ่งที่มีอยู่วันนี้ (อ่านโค้ดจริงแล้ว)

| จุด | ของจริงในโค้ด |
|---|---|
| Template | มีตัวเดียว `templates/hexagonal/` embed ด้วย `include_dir!` |
| Composition root | `src/bootstrap.rs` — `build_services_from_env()` resolve resource (`Db`) ครั้งเดียว แล้วมี `build_<feature>_service(db)` ต่อ feature |
| Adapter selection | ใช้ env: `DATABASE_URL` ตั้ง → Diesel, ไม่ตั้ง → in-memory (`bootstrap.rs:21-23`, `48-54`) |
| DB plumbing | `infrastructure/diesel_db.rs` — hard-code `SqliteConnection`: `SqlitePool`, `SqlitePragmas`, `build_pool()` (WAL + `run_pending_migrations`), `Db::run()` = `spawn_blocking` + checkout + error map |
| Wiring กลไก | `scaffold.rs` — anchor-based ล้วน: `insert_after_line` / `insert_before_line` / `insert_pub_mod` + append ท้ายไฟล์; anchor หายเมื่อไหร่ = fail พร้อมชี้ README |
| Anchor ที่ใช้อยู่ | `user: build_user_service(db.as_ref()),` · `user: build_user_service(None),` · `routes.routes()` · `pub struct Services {` · `.with_state(services)` · block `pub mod` |
| Dry-run | คำนวณทั้ง change ใน memory ก่อน เขียนทีเดียวตอนท้าย → dry-run เป็น code path เดียวกัน |
| Cargo.lock | template ฝัง lock (189 packages) และ `render_tree` rewrite ชื่อ crate ให้ → `cargo build --locked` + Docker ใช้ได้ทันที |
| Runtime image | `Dockerfile` runtime stage = `distroless/cc` (ไม่มี libpq/libmysqlclient) |

**ข้อสังเกตที่กำหนดดีไซน์ทั้งแผน:** ทุกอย่างในระบบ scaffold ปัจจุบันเป็น *additive* (สร้างไฟล์ใหม่ + แทรกบรรทัด)
มีที่เดียวที่จะ additive ไม่ได้คือ `diesel_db.rs` เพราะผูกกับ `SqliteConnection` ตรง ๆ — ดูข้อ 3.4

---

## 1. Decision: CLI UX

### เลือก: `kodegen add <capability>` เป็น implementation เดียว + `new --with` เป็น sugar ที่เรียกมันซ้ำ

```bash
kodegen add postgres                 # relational (ORM ตาม default = diesel)
kodegen add postgres --orm seaorm    # --orm มีผลเฉพาะ capability กลุ่ม relational
kodegen add redis                    # ครอบ Valkey ด้วย (protocol เดียวกัน)
kodegen add s3                       # RustFS / MinIO / AWS
kodegen add --list                   # capability ที่รองรับ + ที่โปรเจกต์นี้เปิดอยู่แล้ว
kodegen add redis --dry-run          # เหมือน feature --dry-run เป๊ะ
kodegen new svc --with postgres,redis   # = new + add วนลูป
```

**เหตุผล**

1. **capability ต้องเพิ่มทีหลังได้** — โปรเจกต์จริงเริ่มด้วย SQLite แล้วค่อยย้าย Postgres เดือนหน้า
   ถ้าออกแบบเป็น flag ของ `new` อย่างเดียว เท่ากับบังคับให้ตัดสินใจวันแรก และคนที่เปลี่ยนใจต้อง
   generate ใหม่ทั้งโปรเจกต์ — ผิดเจตนารมณ์ additive
2. **`new --with` เป็นแค่ loop** — โค้ดจริงประมาณ 10 บรรทัด ไม่ต้องมี code path ที่สอง
   (`Cmd::New` เรียก `capability::add()` ต่อท้าย `render_tree`) จึงไม่มี drift ระหว่างสองทาง
3. **ไม่ใช่ flag ของ `feature`** — คนละแกน: `feature` = หนึ่ง entity ข้ามทุก layer,
   `capability` = infrastructure ระดับโปรเจกต์ ใช้ร่วมกันทุก feature ถ้ายัดเข้า `feature`
   จะได้ combination explosion ทันที (`feature order --transport grpc --db postgres --cache redis`)

**State: ไม่มีไฟล์ metadata ใหม่ (ไม่มี `kodegen.toml`)**
"โปรเจกต์นี้เปิดอะไรไว้บ้าง" อ่านจาก `default = [...]` ใน `[features]` ของ Cargo.toml ที่ generate ออกมา —
มันเป็น truth อยู่แล้วเพราะเป็นตัวที่ควบคุมว่า dependency ไหนถูก compile ไม่ต้องมี state ซ้ำสองที่ให้เพี้ยนกัน

**กติกาความปลอดภัย**

- `add` ตัวเดิมซ้ำ → error `capability 'redis' already added`
- relational มีได้ **หนึ่งเดียวต่อโปรเจกต์** (`Db` handle เป็น singleton) — `add mysql` ทับ `postgres` → error
  พร้อมบอกวิธีทำมือ; ยังไม่รองรับ multi-DB (ดูข้อ 6 "ไม่ทำ")
- `--dry-run` ใช้โครงเดียวกับ `add_feature` (คำนวณครบใน memory → validate anchor ครบ → ค่อยเขียน)

---

## 2. Decision: Crate / driver matrix

ตรวจสอบสถานะ ณ ก.ค. 2026 แล้ว **จุดสำคัญคือหลายตัวเลือกของผู้ใช้ยุบรวมเป็น adapter เดียวได้**

### 2.1 ตารางจริง

| ตัวเลือกผู้ใช้ | crate | หมายเหตุที่ต้องรู้ |
|---|---|---|
| Diesel + SQLite | `diesel 2.3` (`sqlite`,`r2d2`) + `libsqlite3-sys/bundled` | **มีอยู่แล้ว = default** |
| Diesel + PostgreSQL | `diesel/postgres` + `pq-sys{bundled}` + `openssl-sys{vendored}` | diesel ไม่มี bundled ของตัวเอง ต้องพึ่ง sys crate; ไม่ bundle = `distroless/cc` runtime พังตอน run เพราะไม่มี libpq |
| Diesel + MySQL | `diesel/mysql` + `mysqlclient-sys{bundled}` | เหตุผลเดียวกัน |
| Diesel + **MariaDB** | **= MySQL ทุกประการ** | protocol/driver/feature เดียวกัน ต่างแค่ docker image + doc → **ไม่ใช่ adapter ใหม่** |
| Diesel + MSSQL | **ไม่มี** | diesel ไม่รองรับ SQL Server (issue #2201 ค้างมาตั้งแต่ 2019) → ต้อง `tiberius 0.12` เขียน SQL เอง ไม่มี ORM |
| SeaORM + pg/mysql/sqlite | `sea-orm 2.0` (ออก ม.ค. 2026) | pure Rust ผ่าน sqlx + rustls → **ไม่มีปัญหา C lib / distroless เลย** |
| SeaORM + MSSQL | **ไม่มีใน open source** | มีเฉพาะ SeaORM X (commercial) → นอกขอบเขต |
| MongoDB | `mongodb 3.5` official, pure Rust | |
| ClickHouse | `clickhouse 0.15` official (ClickHouse/clickhouse-rs) | pure Rust, typed, มี mock ให้เทสต์ |
| Redis | `redis 1.x` (`tokio-comp`,`connection-manager`) | |
| **Valkey** | **= `redis` crate ตัวเดิม** | redis-rs ระบุรองรับ Redis 6+/Valkey 7+ (RESP) → **ไม่ใช่ adapter ใหม่** |
| InfluxDB | line protocol ผ่าน HTTP (`reqwest` + `influxdb-line-protocol` สำหรับ escaping) | `influxdb2` = community fork; `influxdb3_client` ยัง unstable และยังไม่ official — จึงไม่ผูกกับ crate ใดตัวหนึ่ง |
| **GreptimeDB** | **รับ InfluxDB line protocol ได้ + พูด MySQL/PG wire protocol ได้** | `greptimedb-ingester 0.15` (gRPC/Arrow Flight) ไว้ค่อยเพิ่มเมื่อต้องการ throughput สูงจริง → **ยังไม่ใช่ adapter ใหม่** |
| **TimescaleDB** | **= PostgreSQL driver ตัวเดิม** | เป็น extension ของ Postgres ต่างแค่ migration (`CREATE EXTENSION timescaledb; SELECT create_hypertable(...)`) → **ไม่ใช่ adapter ใหม่** |
| RustFS | `aws-sdk-s3` (custom `endpoint_url` + `force_path_style`) | RustFS = S3-compatible (SigV4) ใช้ SDK มาตรฐานได้; ครอบ MinIO/Ceph/AWS ฟรี |

### 2.2 การยุบรวม (สาระสำคัญของแผนนี้)

ผู้ใช้เห็น 5 กลุ่ม/16 ตัวเลือก แต่ของที่ต้องเขียนจริงมี **7 adapter**:

```
relational : diesel_sqlite | diesel_pg | diesel_mysql        (MariaDB ⊂ mysql, Timescale ⊂ pg)
             (+ seaorm_* ทีหลัง, + tiberius แยกวงเพราะไม่มี ORM)
cache      : redis                                            (Valkey ⊂ redis)
document   : mongodb
analytics  : clickhouse
timeseries : line_protocol                                    (InfluxDB v2/v3 + GreptimeDB)
blob       : s3                                               (RustFS + MinIO + AWS)
```

⚠️ **ช่องโหว่ของ matrix ที่ต้องบอกผู้ใช้ตรง ๆ:** "ORM × DB" ไม่ใช่ตารางเต็ม
MSSQL ไม่มีทั้ง Diesel และ SeaORM (open source) → เลือก MSSQL = ไม่ได้ ORM ต้องเขียน SQL ผ่าน tiberius
CLI ต้อง error ให้ชัดตอนใส่ combination ที่ไม่มีจริง ไม่ใช่ generate โค้ดที่ compile ไม่ผ่าน

### 2.3 ผลข้างเคียงที่ต้องชั่ง: Diesel vs SeaORM

Diesel+pg/mysql มีภาษี C library (`pq-sys{bundled}` ต้องมี `openssl-sys{vendored}` → build ช้าลงมาก
และ build stage ต้องมี perl) ส่วน SeaORM/sqlx เป็น pure Rust + rustls ไม่มีภาษีนี้เลย

เลือก **Diesel ก่อน** เพราะเป็นของที่ template ใช้อยู่ = diff เล็กสุด (สลับ connection type ไม่ต้องรื้อ
`Db::run`/`spawn_blocking`/port) แต่ถ้าหลัง Phase 1 พบว่า build time บน CI/Docker เจ็บจริง
ให้เลื่อน SeaORM ขึ้นมาเป็น Phase ถัดไปทันที — นี่คือ signal ที่ชัดที่สุดที่จะทำให้ Phase 6 คุ้ม

---

## 3. Decision: โครงสร้างใน template / scaffold

### 3.1 ไฟล์ที่ generate ต่อ capability

| capability | ไฟล์ใหม่ | port | default adapter (ไม่ต้องมี server) |
|---|---|---|---|
| relational (pg/mysql) | *แทนที่* `infrastructure/diesel_db.rs` + `migrations/<seed>/up.sql`,`down.sql` | ใช้ `UserRepository` เดิม | `in_memory_repo` เดิม (ไม่ตั้ง `DATABASE_URL`) |
| mongodb | `infrastructure/mongo_repo.rs` | ใช้ `UserRepository` เดิม | เหมือนกัน |
| redis | `infrastructure/redis_cache.rs` + `infrastructure/in_memory_cache.rs` | **`Cache` (ใหม่)** | `in_memory_cache` |
| clickhouse | `infrastructure/clickhouse_events.rs` + `infrastructure/null_events.rs` | **`EventSink` (ใหม่)** | `null_events` |
| timeseries | `infrastructure/line_protocol_events.rs` | **`EventSink` (ตัวเดียวกัน)** | `null_events` |
| s3 | `infrastructure/s3_blobs.rs` + `infrastructure/fs_blobs.rs` | **`BlobStore` (ใหม่)** | `fs_blobs` (tempdir) |
| ทุกตัว | `tests/<capability>.rs` (env-gated) · block ใน `docker-compose.dev.yml` | | |

**port ใหม่รวมทั้งหมด 3 ตัวเท่านั้น: `Cache`, `EventSink`, `BlobStore`**
เขียนต่อท้าย `domain/ports.rs` + re-export ต่อท้าย `domain/mod.rs` (append-only ทั้งคู่ ใช้กลไกเดิม)

เหตุผลที่ relational + mongodb **ไม่สร้าง port ใหม่**: มันคือ adapter อีกตัวของ `UserRepository` ที่มีอยู่
→ ได้ test เดิมคุ้มฟรี และพิสูจน์ substitutability ตามที่ template โฆษณาไว้

เหตุผลที่ ClickHouse + time-series ใช้ port เดียวกัน (`EventSink { write(Vec<Event>) }`):
shape ของ write path เหมือนกันจริง (append-only, timestamp + tags + fields) การแยกสอง trait ที่หน้าตา
เท่ากันคือ abstraction ที่ไม่มีใครขอ ถ้าใครต้องการ **query** เชิงวิเคราะห์จาก ClickHouse นั่นคือ port
คนละตัวและอยู่นอกขอบเขต (ดูข้อ 6)

**ทุก capability ต้องมี default adapter ที่ไม่ต้องพึ่ง server** — นี่คือสิ่งที่รักษาสัญญา
"generate แล้ว build/test ผ่านทันที": ไม่ตั้ง env → ได้ in-memory/no-op/tmpdir เหมือนที่ `DATABASE_URL` ทำอยู่

### 3.2 Anchor ที่ต้องเพิ่ม/เปลี่ยนใน template

เพิ่ม comment anchor **3 จุด** ใน `templates/hexagonal/src/bootstrap.rs`:

```rust
pub fn build_services_from_env() -> api::Services {
    let db = ...;
    // kodegen:resources            <-- (1) let redis = ...; let s3 = ...;
    api::Services {
        user: build_user_service(db.as_ref()),
        greeter: build_greeter(),
        // kodegen:services:env      <-- (2) cache: build_cache(redis.as_ref()),
    }
}

pub fn build_services() -> api::Services {
    api::Services {
        user: build_user_service(None),
        greeter: build_greeter(),
        // kodegen:services:default  <-- (3) cache: build_cache(None),
    }
}
```

- ไม่ต้องมี anchor สำหรับ import: builder ที่ generate ใช้ path เต็ม (`crate::infrastructure::redis_cache::RedisCache`)
  ตามแบบที่ `BOOTSTRAP_TPL` ทำอยู่แล้ว → ประหยัด anchor ไปหนึ่งจุด
- `pub struct Services {` ใน `api/mod.rs` ใช้ anchor เดิมที่ `feature` ใช้อยู่ (เพิ่ม field)
- `infrastructure/mod.rs`, `domain/mod.rs`, `domain/ports.rs` = append อย่างเดียว ไม่ต้องมี anchor
- `Cargo.toml` anchor ใหม่: บรรทัด `default = [` ใน `[features]`
- `docker-compose.dev.yml` anchor ใหม่: บรรทัด `services:`

**งานพ่วงที่ควรทำพร้อมกัน (Phase 1):** ย้าย `scaffold.rs` ให้ใช้ `// kodegen:services:*` เป็น anchor หลัก
แล้ว **fallback ไป anchor เดิม** (`user: build_user_service(...)`) ถ้าไม่เจอ — ประมาณ 10 บรรทัด
ได้สองอย่าง: (ก) โปรเจกต์ที่ generate ไปแล้วก่อนหน้านี้ยัง `kodegen feature` ได้ (ข) ลบ feature `user`
ทิ้งแล้ว scaffolder ไม่พังอีกต่อไป

### 3.3 Env vars (คงกติกาเดิม: ไม่ตั้ง = default adapter)

| capability | env |
|---|---|
| relational | `DATABASE_URL` (เดิม — เปลี่ยนแค่รูปแบบ URL) |
| redis/valkey | `REDIS_URL` |
| mongodb | `MONGODB_URL`, `MONGODB_DB` |
| clickhouse | `CLICKHOUSE_URL`, `CLICKHOUSE_DB` |
| timeseries | `TSDB_URL`, `TSDB_TOKEN`, `TSDB_BUCKET` (ชื่อกลาง เพราะปลายทางเป็น Influx หรือ Greptime ก็ได้) |
| s3 | `S3_ENDPOINT`, `S3_BUCKET`, `S3_REGION`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` |

`add` พิมพ์ env ที่ต้องใช้ออก stdout ตอนจบ (แบบเดียวกับที่ `feature` พิมพ์ route) และ append section
"Capabilities" ต่อท้าย README ของโปรเจกต์ — append-only ไม่ต้องมี anchor

### 3.4 Migration ต่อ DB — จุดที่ additive ไม่ได้ 100%

`diesel_db.rs` ผูกกับ `SqliteConnection` ทั้งไฟล์ (`SqlitePool`, `SqlitePragmas`, WAL setup) จึง **แทนที่ทั้งไฟล์**
ไม่ใช่แทรกบรรทัด นี่เป็น non-additive edit จุดเดียวของแผนทั้งหมด

การป้องกัน: ก่อนแทนที่ ตรวจ marker line (`pub type SqlitePool = Pool<ConnectionManager<SqliteConnection>>;`)
ถ้าไม่ตรง = ผู้ใช้แก้ไฟล์เองไปแล้ว → **refuse** พร้อมพิมพ์ 4 ขั้นตอนที่ต้องทำมือ (เหมือนพฤติกรรม anchor ปัจจุบัน)
ห้าม overwrite เงียบ ๆ

migration ต่อ dialect:

| DB | วิธี |
|---|---|
| SQLite (default) | `diesel_migrations::embed_migrations!` ตามเดิม |
| PostgreSQL | เดิม แต่ `add` เขียนทับ seed `up.sql` เป็น dialect pg (`TEXT PRIMARY KEY`) |
| MySQL/MariaDB | เขียนทับเป็น `VARCHAR(64) PRIMARY KEY` — **MySQL index `TEXT` ตรง ๆ ไม่ได้ ต้องมี prefix length** (ถ้าลืมข้อนี้ migration พังตอน runtime ไม่ใช่ตอน compile) |
| TimescaleDB | pg migration + migration ที่สอง: `CREATE EXTENSION IF NOT EXISTS timescaledb;` + `create_hypertable` |
| MongoDB | ไม่มี migration — สร้าง index ตอน startup |
| ClickHouse | `CREATE TABLE IF NOT EXISTS events (...) ENGINE = MergeTree` ตอน startup |
| Influx/Greptime | schemaless ตอน write |
| S3 | สร้าง bucket ถ้ายังไม่มี ตอน startup |
| MSSQL (tiberius) | ไม่มี migration framework → ship `schema.sql` รันตอน startup แบบ idempotent |

`add` จะ **แตะเฉพาะ seed migration ที่ kodegen เป็นเจ้าของ** ถ้าเจอ migration dir อื่นที่ผู้ใช้เขียนเอง
ให้ warn ว่า "ตรวจ dialect ของ migration ที่คุณเขียนเองด้วย" — ไม่แตะ ไม่แปลง

### 3.5 docker-compose — **ต้องมี**

สร้าง `docker-compose.dev.yml` (dev-only, แยกจาก `Dockerfile` ของ production) พร้อม `services:` anchor
`add` แต่ละครั้ง append หนึ่ง block + healthcheck คุ้มมากเพราะมันคือ **ตัวเดียวกับที่ integration test
และ CI job ใช้ boot server** — ทำครั้งเดียวได้สองงาน

---

## 4. Decision: Cargo.toml / Cargo.lock strategy

### เลือก: optional dependency + cargo feature ที่ประกาศไว้ล่วงหน้าครบทุก capability, `add` แค่พลิก `default = [...]`

```toml
[dependencies]
diesel = { version = "2.3.11", features = ["r2d2"] }
libsqlite3-sys = { version = "0.37", features = ["bundled"], optional = true }
pq-sys        = { version = "0.7", features = ["bundled"], optional = true }
redis         = { version = "1", features = ["tokio-comp"], optional = true }
mongodb       = { version = "3.5", optional = true }
clickhouse    = { version = "0.15", optional = true }
aws-sdk-s3    = { version = "1", optional = true }
# ...

[features]
default   = ["sqlite"]                       # <-- state ของโปรเจกต์อยู่ตรงนี้ที่เดียว
sqlite    = ["diesel/sqlite", "dep:libsqlite3-sys"]
postgres  = ["diesel/postgres", "dep:pq-sys", "dep:openssl-sys"]
mysql     = ["diesel/mysql", "dep:mysqlclient-sys"]
cache     = ["dep:redis"]
blob      = ["dep:aws-sdk-s3"]
```

### ทำไมต้องแบบนี้ (ทดสอบจริงแล้ว ไม่ได้เดา)

รัน experiment จริงกับ cargo:

| ทดสอบ | ผล |
|---|---|
| `redis` เป็น `optional = true`, `default = []` → `cargo generate-lockfile` | **redis 1.4.1 อยู่ใน Cargo.lock** (54 packages) แม้ feature ปิดอยู่ |
| `cargo build --locked` (default) | ผ่าน และ **ไม่ compile redis** |
| `cargo build --locked --features cache` | ผ่าน **โดยไม่แตะ lock เลย** |
| แก้ `default = ["cache"]` แล้ว `cargo build --locked --offline` | ผ่าน lock ไม่เปลี่ยน |
| เพิ่ม `mongodb = "3"` เป็น dep ปกติที่ไม่มีใน lock แล้ว `--locked` | **พัง**: `cannot update the lock file ... because --locked was passed` |

สรุปเชิงวิศวกรรม: **lock ครอบ optional dep ทั้งหมดอยู่แล้ว การเปิด feature ไม่ทำให้ lock เพี้ยน แต่การ
เพิ่ม dependency ใหม่ตอน scaffold ทำให้ `--locked` พังทันที** → ทางเลือกอื่นถูกตัดทิ้งด้วยหลักฐาน:

- ❌ `add` รัน `cargo add` — ต้องมีเน็ต, ผลลัพธ์ไม่ deterministic, พังในโหมด offline ที่ template โฆษณาไว้
- ❌ ship lock ที่เป็น superset (มี dep เกินกว่าที่ manifest ประกาศ) — cargo จะ prune → `--locked` พังตั้งแต่ default
- ✅ optional dep + feature — offline ได้, deterministic, `--locked` ผ่านทุก configuration

### ผลพลอยได้ / ราคาที่ต้องจ่าย

- lock ของ template จะโตจาก ~189 → ประมาณ 600-800 packages (ไฟล์ใหญ่ขึ้น ~10 เท่า แต่ embed ในไบนารีเฉย ๆ)
- kodegen CI ต้องมี job: copy template → `cargo generate-lockfile` → `git diff --exit-code Cargo.lock`
  (กัน lock เก่ากว่า manifest แบบเงียบ ๆ)
- ต้องมี job build **ทุก feature เปิดพร้อมกัน** อย่างน้อยหนึ่งครั้ง เพื่อจับ feature unification / TLS stack ชนกัน
  (tiberius default ใช้ native-tls, ที่เหลือ rustls → บังคับ tiberius ใช้ feature `rustls`)
- **ตัดสินใจโดยเจตนา: ไม่ใส่ `#[cfg(feature = ...)]` ในโค้ดที่ generate** — kodegen เขียนไฟล์ adapter
  ก็ต่อเมื่อเปิด capability นั้น การใส่ cfg จะลามเข้า `bootstrap.rs` ทุกบรรทัดโดยไม่ได้อะไรเพิ่ม
  ข้อแลก: โปรเจกต์ที่ generate แล้วจะ build ด้วย `--no-default-features` ไม่ได้ (มันเป็น app ไม่ใช่ library)
  ถ้าวันหนึ่งมีคนต้องการจริง ค่อยเติม cfg เฉพาะบรรทัด `pub mod` + builder

### Dockerfile / CI ของโปรเจกต์ที่ generate

runtime stage เป็น `distroless/cc` → **ห้ามใช้ dynamic libpq/libmysqlclient** ไม่งั้นพังตอน run ไม่ใช่ตอน build
จึงบังคับใช้ `pq-sys{bundled}` / `mysqlclient-sys{bundled}` (แนวเดียวกับ `libsqlite3-sys{bundled}` ที่ template
ทำอยู่แล้ว) แลกกับ build stage ช้าลง + ต้องมี perl สำหรับ `openssl-sys{vendored}` →
`add postgres` ต้องแทรก `perl` เข้าบรรทัด `apt-get install` ใน Dockerfile (anchor: `protobuf-compiler`)

---

## 5. Decision: Testing strategy

### 5.1 unit test ของ kodegen (เร็ว, รันทุกครั้ง)

ต่อยอด fixture ใน `scaffold.rs::tests` (เพิ่ม `Cargo.toml` ที่มี `[features]` + bootstrap ที่มี comment anchor):

- dry-run ไม่เขียนไฟล์ / plan ตรงกับ real run — pattern เดิม ใช้กับ `add` ด้วย
- `add` ซ้ำ → error, relational ตัวที่สอง → error
- `default = [...]` ถูกแก้ถูกต้อง และ **ไม่แตะ `[dependencies]`** (assert ตรง ๆ — นี่คือสัญญาเรื่อง lock)
- anchor หาย → error (รวม fallback path ของ anchor เก่า: fixture แบบ legacy ต้องยัง `feature` ได้)
- `diesel_db.rs` ถูกแก้มือแล้ว → refuse ไม่ overwrite

### 5.2 e2e ของ kodegen (ช้า, `#[ignore]` + job แยกใน CI)

**ต้องมี** เพราะเป็นการทดสอบเดียวที่พิสูจน์ข้ออ้างเรื่อง lock:

```
new demo → add <capability> → cargo build --locked
                            → cargo fmt --check
                            → cargo clippy --all-targets -- -D warnings
                            → cargo test
```

matrix = หนึ่ง job ต่อ capability (ไม่ทำ cross product — ดูข้อ 6) + หนึ่ง job "เปิดทุกตัวพร้อมกัน"

### 5.3 test ของ adapter ในโปรเจกต์ที่ generate

สองชั้น:

1. **ชั้นที่รันเสมอ** — เทสต์ default adapter (in-memory / no-op / tmpdir) ไม่ต้องมี server
   → CI ของโปรเจกต์ที่ generate ยังเขียวโดยไม่ต้องแก้อะไร
2. **ชั้นที่ต้องมี server** — เทสต์จริงกับ Redis/Mongo/ClickHouse/RustFS

วิธี skip: **ใช้ env var เป็นตัวตัดสิน ไม่ใช้ `#[ignore]`**

```rust
let Ok(url) = std::env::var("REDIS_URL") else {
    eprintln!("REDIS_URL unset — skipping redis integration test");
    return;
};
```

เหตุผล: (ก) สอดคล้องกับกลไก adapter selection ของ template ที่ใช้ env อยู่แล้ว
(ข) `#[ignore]` ต้องจำ flag เพิ่มตอนรัน ส่วน env-gate เปิดใช้ได้ด้วยการ `docker compose -f docker-compose.dev.yml up -d`
เฉย ๆ (ค) ไม่ต้องมี CI job ที่สองสำหรับโปรเจกต์ผู้ใช้ — job เดิม job เดียว ตั้ง env เมื่อไหร่ก็เทสต์ลึกขึ้นเอง

ข้อแลกที่รับได้: เทสต์ที่ skip ยังนับเป็น pass (เขียว) — ถ้าวันหนึ่งกลัว false green ค่อยเพิ่ม
`CI_REQUIRE_SERVICES=1` ให้ panic แทน return

---

## 6. Phasing

หลักการเรียง: **ทำสิ่งที่ reuse ของเดิมมากที่สุดและเสี่ยงต่ำที่สุดก่อน** เพื่อพิสูจน์กลไก (feature/lock/anchor/compose)
ด้วยงานที่เล็กที่สุด แล้วค่อยเพิ่มของที่ต้องเขียนใหม่ทั้งก้อน

| Phase | ขอบเขต | ขนาด | เกณฑ์จบ |
|---|---|---|---|
| **1** | รากฐาน + relational (Diesel): comment anchors + fallback ใน `scaffold.rs`, `[features]` + lock ใหม่ใน template, `kodegen add` + `new --with`, `add postgres/mysql/mariadb/sqlite`, migration ต่อ dialect, compose, Dockerfile apt patch, e2e harness | **M** | `add postgres` → generated project ผ่าน fmt/clippy/test + `build --locked`; ไม่ตั้ง `DATABASE_URL` ยังได้ in-memory เหมือนเดิม; โปรเจกต์เก่ายัง `kodegen feature` ได้ |
| **2** | `add redis` (ครอบ Valkey) — port `Cache` + in-memory default + redis adapter | **S** | พิสูจน์เส้นทาง "port ใหม่ + field ใน Services" ที่ Phase 3-5 ใช้ซ้ำทั้งหมด |
| **3** | `add s3` (RustFS/MinIO/AWS) — port `BlobStore` + fs default + aws-sdk-s3 | **M** | ระวัง compile time ของ aws-sdk; ถ้าเจ็บเกินไปค่อยพิจารณา `opendal` |
| **4** | `add mongodb` — adapter ตัวที่สองของ `UserRepository` เดิม | **M** | ได้ test เดิมคุ้มฟรี ไม่มี port ใหม่ |
| **5** | analytics/time-series — port `EventSink` + `clickhouse` + `line_protocol` (Influx v2/v3 + Greptime) + `timescaledb` = pg + hypertable migration | **M** | 4 ตัวเลือกของผู้ใช้ จบด้วย 2 adapter |
| **6** | SeaORM เป็นทางเลือก ORM | **L** | **รอ signal** — เป็นการ implement ทุกอย่างใน Phase 1 ซ้ำอีกรอบ (repo/pool/migration คนละแบบ, async ไม่ต้อง spawn_blocking) จะทำเมื่อ (ก) มีคนขอ ≥2 ราย หรือ (ข) ภาษี C lib ของ Diesel+pg ทำให้ build จริงเจ็บ |
| **7** | MSSQL ผ่าน tiberius | **L** | **รอ signal** — ไม่มี ORM ให้ใช้เลย ต้องเขียน SQL เอง + migration เอง + TLS stack คนละตัว + image ทดสอบ 1.5GB ติด EULA รัน CI ฟรีไม่ได้ ต้นทุนสูงสุด reuse ต่ำสุด |

### สิ่งที่จะ *ไม่* ทำ (ตั้งใจ)

- ❌ **template แยกต่อ combination** — ข้อจำกัดที่ตั้งไว้ตั้งแต่ต้น และเป็นเหตุผลที่ทุกอย่างข้างบนเป็น feature flag
- ❌ **`kodegen.toml` / state file** — `default = [...]` เป็น truth อยู่แล้ว
- ❌ **abstraction ครอบ ORM** (trait `Orm`) — `UserRepository` ใน domain คือ abstraction ที่มีอยู่แล้ว
  การมี trait ครอบ Diesel/SeaORM อีกชั้นคือ interface ที่มี implementation เดียวต่อโปรเจกต์
- ❌ **relational หลายตัวพร้อมกัน / read-replica / sharding** — `Db` เป็น singleton, ยังไม่มีใครขอ
- ❌ **GreptimeDB native ingester** — line protocol ครอบได้แล้ว จะเพิ่มเมื่อมีคนบอกว่า throughput ไม่พอจริง
- ❌ **ClickHouse query/analytics port** — ทำเฉพาะ write path; query เชิงวิเคราะห์เป็นงานของแต่ละโปรเจกต์
- ❌ **SeaORM X / MSSQL commercial**
- ❌ **cross-product matrix ใน CI** (6 DB × 2 ORM × 4 store) — เทสต์ capability × default เท่านั้น + หนึ่ง job "เปิดหมด"
- ❌ **`--transport` × `--db` combination ใน `feature`** — คนละแกน ไม่ผสม

---

## 6.1 CTO revisions (approve-with-changes)

Phase 1 ถูกแตกเป็นสองก้อน เพราะก้อนเดียวกินทั้ง anchor + lock + `kodegen add` + migration
ต่อ dialect + compose พร้อมกัน — review ยาก และถ้า lock strategy ผิดจะรู้ตอนท้ายสุด

| Phase | ขอบเขต | gate ที่ต้องผ่านถึงจะขึ้น phase ถัดไป |
|---|---|---|
| **1a** (รากฐาน) | comment anchors ใน template + fallback ใน `scaffold.rs`, `[features]` + optional deps (เฉพาะที่ 1b ใช้: diesel postgres/mysql + sys crate bundled), regenerate lock, `cargo audit` ใน CI ของ template | `cargo build --locked` ผ่านด้วย default; `--features postgres` resolve/compile ได้โดย **lock ไม่เปลี่ยน**; โปรเจกต์ที่ generate ก่อนมี marker ยัง `kodegen feature` ได้ (fallback) |
| **1b** (ของจริง) | `kodegen add` + `new --with`, `add postgres/mysql/mariadb/sqlite`, เขียนทับ `diesel_db.rs` + marker check, migration ต่อ dialect, `docker-compose.dev.yml`, Dockerfile apt patch (perl), e2e harness, README ทั้งสองไฟล์ | ตามเกณฑ์จบเดิมของ Phase 1 |

**เพิ่มโดย CTO**

- **error ตอน marker หาย ต้องบอกชื่อ marker และบรรทัดที่ควรใส่คืน** ไม่ใช่แค่ "anchor not found" —
  ผู้ใช้ที่ลบ marker ทิ้งโดยไม่ตั้งใจต้องซ่อมเองได้จากข้อความ error ล้วน ๆ (ทำแล้วใน 1a)
- **`cargo audit` ใน CI ของ template** — dependency tree กำลังจะโตขึ้นหลายเท่าจาก optional deps
  จึงต้องมีตาข่ายรับ advisory ตั้งแต่ก่อนโตจริง ใส่เป็น job แยกไม่ให้บัง signal ของ fmt/clippy/test (ทำแล้วใน 1a)
- **scheduled lock-refresh job — บันทึกไว้ ยังไม่ implement** (งานของ devops หลัง 1b):
  cron รายสัปดาห์ใน CI ของ kodegen เอง → `cargo update` ใน `templates/hexagonal` → เปิด PR
  เหตุผล: lock ที่ฝังใน binary จะเก่าเงียบ ๆ ระหว่างที่ไม่มีใครแตะ template และ `cargo audit`
  จะเริ่มแดงจาก dependency ของ capability ที่ไม่มีใครใช้ — ต้องมีคนไล่ให้เป็นรอบ ไม่ใช่ตอนเจอปัญหา

---

## 7. ความเสี่ยงที่เหลือ / ต้องเฝ้า

1. **lock ที่โตขึ้นกลายเป็นภาระ maintenance** — `cargo update` ครั้งเดียวกระทบ dependency ของ capability
   ที่แทบไม่มีใครใช้ ทางลด: CI job ตรวจ lock สดทุก PR (ข้อ 4) และไม่เพิ่ม capability ที่ยังไม่มีคนขอ
2. **`openssl-sys{vendored}` ทำ build stage ช้าและเปราะ** (ต้องมี perl) — ถ้าเจ็บจริง = signal ให้ดัน Phase 6 ขึ้นมา
3. **`diesel_db.rs` เป็นการเขียนทับ ไม่ใช่แทรก** — กันด้วย marker check แต่ยังเป็นจุดที่ผู้ใช้ที่แก้ไฟล์เองจะโดน refuse
   (ตั้งใจให้ refuse ดีกว่า clobber)
4. **`EventSink` เป็น port เดียวสำหรับ ClickHouse + time-series** — ถ้ามี requirement ที่ shape ต่างจริง
   (เช่นต้องการ typed row ของ ClickHouse) ให้แตกเป็นสอง port ตอนนั้น ไม่ต้องเผื่อไว้ก่อน
5. **MySQL `VARCHAR(64)` vs SQLite/PG `TEXT`** — เป็น runtime failure ไม่ใช่ compile failure
   → e2e job ของ mysql ต้องรัน migration จริงกับ server จริง ไม่ใช่แค่ compile
6. **`--no-default-features` ใช้ไม่ได้กับโปรเจกต์ที่ generate** — ยอมรับโดยเจตนา (ข้อ 4)

---

## ภาคผนวก: การเปลี่ยนแปลงใน `src/` ของ kodegen เอง

| ไฟล์ | การเปลี่ยนแปลง |
|---|---|
| `src/main.rs` | เพิ่ม `Cmd::Add { capability, orm, path, dry_run }`, เพิ่ม `--with` ให้ `New` (loop เรียก `capability::add`) |
| `src/scaffold.rs` | เปลี่ยน `edit` / `insert_after_line` / `insert_before_line` / `insert_pub_mod` / `anchor_err` เป็น `pub(crate)`; เพิ่ม anchor fallback (comment anchor ก่อน แล้วค่อย legacy) |
| `src/capability.rs` (ใหม่) | `add()` โครงเดียวกับ `add_feature()` เป๊ะ (คำนวณครบใน memory → validate → เขียนทีเดียว → `cargo fmt`) + template ของแต่ละ adapter |
| `templates/hexagonal/Cargo.toml` | เพิ่ม `[features]` + optional deps ครบทุก capability |
| `templates/hexagonal/Cargo.lock` | regenerate หนึ่งครั้ง (โตขึ้นมาก) |
| `templates/hexagonal/src/bootstrap.rs` | เพิ่ม comment anchor 3 จุด |
| `templates/hexagonal/docker-compose.dev.yml` (ใหม่) | โครงเปล่า + `services:` anchor |
| `templates/hexagonal/README.md` | อธิบาย capability + ตาราง env ที่ `add` จะต่อท้าย |
