# Implementation Plan — Phase 6: SeaORM as a second ORM

สถานะ: **ship ครบทั้ง 6a และ 6b แล้ว** (6a = `44b26bc`, 6b = `8b871a4`)
เอกสารนี้เก็บไว้เป็นบันทึกการตัดสินใจ + §12/§13 ที่บันทึกว่าอะไรขัดกับความจริงตอนรัน
ขอบเขต: ให้ผู้ใช้เลือก ORM ได้บน capability กลุ่ม relational โดยไม่แตะ port,
ไม่แตะ `kodegen feature`, และไม่แตะ composition root ที่เป็น `fn`

> หมายเหตุ (ตอนเขียนแผนนี้ — ปัจจุบันทั้งสองงาน ship แล้ว): เขียนเป็น logical ล้วน ไม่อ้าง line number
> — งาน `add greptimedb` กำลังแตะ `capability.rs` อยู่ และแผนนี้จะถูก implement หลังงานนั้น commit
> (ลำดับจริง: greptimedb `49f4186` ก่อน, seaorm 6a `44b26bc` ตามหลัง — ตรงตามที่ตั้งใจไว้)

---

## 0. ข้อสรุปที่กลับหัวการประเมินเดิม

แผนเดิมตี Phase 6 = **L** ด้วยเหตุผล "implement ทุกอย่างใน Phase 1 ซ้ำอีกรอบ"
ตอนที่เขียนบรรทัดนั้น **`add mongodb` ยังไม่มี** — แต่ตอนนี้มีแล้ว และมันคือ
เส้นทาง "datastore ที่ไม่ใช่ Diesel" ที่ SeaORM ต้องการเป๊ะ ๆ:

| สิ่งที่ Phase 6 เคยต้องสร้างเอง | ของที่มีอยู่แล้ววันนี้ |
|---|---|
| กติกา "ORM สองตัวอยู่ด้วยกันไม่ได้" | `Slot::Datastore` — ใส่ feature name เพิ่มก็จบ |
| สลับ composition root ออกจาก Diesel | `rewrite_bootstrap` (สาม block, verbatim หรือ refuse) |
| adapter ที่ connect แบบ async ใน `fn` | `Mongo` = `Arc<OnceCell<Client>>` — พิสูจน์แล้ว |
| ไฟล์ Diesel ที่ไม่ได้ใช้ต้องหายไปไหน | `#[cfg(feature = "relational")]` ใน `infrastructure/mod.rs` |
| pool + `spawn_blocking` + r2d2 | **ไม่ต้องมี** — sea-orm ถือ pool ของตัวเอง |
| migration harness | **ไม่ทำ** (ดู §5) — ตาม precedent ของ mongo/clickhouse/s3 |
| Dockerfile apt patch (perl/make/cmake) | **ไม่ต้องมี** — pure Rust ทั้งสาย |

เหลืองานจริงคือ *สองไฟล์ template* + *หนึ่ง flag* + *สาม feature line*
→ ประเมินใหม่: **6a = M, 6b = S** ไม่ใช่ L ก้อนเดียว

---

## 1. ข้อเท็จจริงที่ยืนยันแล้ว (crates.io sparse index, 2026-07-28)

ตรวจจาก `index.crates.io/se/a-/sea-orm` โดยตรง ไม่ใช่จากความจำ:

| ข้อ | ค่าจริง | ผลต่อแผน |
|---|---|---|
| sea-orm ล่าสุด | **2.0.0**, `rust-version = 1.94.0` | **ใช้ไม่ได้** — template อยู่ที่ 1.88 |
| 1.x ล่าสุด | **1.1.20**, `rust-version = 1.81` | ✅ ใช้ตัวนี้ — `sea-orm = "1"` กัน 2.0 ด้วย semver อยู่แล้ว ไม่ต้อง pin พิเศษ |
| default features | `macros, with-json, with-chrono, with-rust_decimal, with-bigdecimal, with-uuid, with-time` | ต้อง `default-features = false` — `chrono`/`bigdecimal`/`rust_decimal` **ยังไม่มีใน lock** และตาราง `users` เป็น TEXT ล้วน |
| backend features | `sqlx-postgres` · `sqlx-mysql` · `sqlx-sqlite` | คนละแกนกับ diesel จริงตามที่คาด |
| runtime/TLS | `runtime-tokio-rustls` → `sqlx/runtime-tokio-rustls` | **บังคับ และลืมแล้วไม่มีใครเตือน**: ไม่ใส่ = compile ผ่าน แล้ว **panic ตอน runtime** (`sqlx-core/src/rt`: "either the runtime-async-std or runtime-tokio feature must be enabled") → ต้องอยู่ใน feature ฐาน ไม่ใช่ต่อ backend |
| TLS provider | `sqlx/runtime-tokio-rustls` → `tls-rustls-ring` → **ring** ส่วน `aws-sdk-s3` ใช้ **aws-lc-rs** | ⚠️ ชนกัน — ดู §3.1 |
| transitive | `sqlx ^0.8.4` (→0.8.6), `sea-query 0.32`, `sea-query-binder 0.7`, `ouroboros`, `strum`, `derive_more`, `async-stream` | lock โตจาก **406** → ประมาณ **465–480** (rustls/ring/aws-lc-rs/uuid/time มีอยู่แล้ว) |
| MSSQL | ไม่มีใน open source | นอกขอบเขต ตามแผนเดิม |

**บทสรุปข้อ 1: SeaORM ลงได้โดยไม่ต้องขยับ `rust-version` เลย** ถ้าใช้ `"1"`
วันที่อยากขึ้น 2.0 คือวันที่ template ขึ้น 1.94 อยู่แล้ว (aws-sdk-s3 ก็รอเรื่องนี้อยู่)
— เขียนไว้เป็น comment ข้าง dependency แบบเดียวกับที่ `clickhouse = "0.13"` ทำ

---

## 2. Decision: CLI shape

### เลือก: `kodegen add <relational> --orm <diesel|seaorm>` (คงรูปแบบตามแผนเดิม)

```bash
kodegen add postgres --orm seaorm          # SeaORM + PostgreSQL
kodegen add mysql --orm seaorm             # SeaORM + MySQL   (6b)
kodegen add mariadb --orm seaorm           # = mysql, คนละ image (6b)
kodegen add timescaledb --orm seaorm       # = postgres, DDL เพิ่ม
kodegen add postgres                       # ยังเป็น Diesel เหมือนเดิม (default)
kodegen new svc --with postgres,redis --orm seaorm
```

**ทางเลือกที่ทิ้ง:** `add seaorm-postgres` เป็น capability ใหม่ →
`Capability` โตจาก 10 เป็น 14 variant, `--list` กลายเป็นกำแพง, และ mariadb/timescaledb
ที่เป็น alias อยู่แล้วจะกลายเป็น alias ยกกำลังสอง — ORM เป็น **แกนที่ตั้งฉาก** กับ capability
เหมือนที่ `--transport rest|grpc` ตั้งฉากกับชื่อ feature จึงควรเป็น flag เหมือนกัน

**รายละเอียดที่ต้องตัดสินให้ครบ**

- `orm: Option<Orm>` ไม่ใช่ `default_value = "diesel"` — ต้องแยก "ไม่ได้ใส่" ออกจาก
  "ใส่ diesel" ให้ได้ ไม่งั้น error ข้อถัดไปเขียนไม่ได้
- `--orm` กับ capability ที่ไม่ใช่ relational → **error ก่อนอ่านไฟล์ใด ๆ**:
  `--orm applies only to relational capabilities (postgres, mysql, mariadb, timescaledb) — 'redis' has no ORM`
- `new --with a,b,c --orm seaorm` → มีผลกับ relational entry ตัวเดียวใน list
  (`check_combination` การันตีอยู่แล้วว่ามีได้ตัวเดียว); ถ้า `--with` ไม่มี relational เลย → error
- `Capability::feature()` ต้องรับ orm: `fn feature(self, orm: Orm) -> &'static str`
  ผลกระทบ ~5 call site (`add`, `check_combination`, `list`, และ test ที่ pin
  `Slot::features()` ↔ `Capability::feature()` — test นั้นต้องวนทั้งสอง ORM)

### `--list`

ไม่แตกบรรทัดเป็นสองเท่า เพิ่ม **สองบรรทัด** ใต้บล็อก relational:

```
  postgres     relational — PostgreSQL   (--orm diesel [default] | seaorm)
  ...
Relational capabilities take --orm: `diesel` links a C driver and gives you
versioned migrations; `seaorm` is pure Rust (no perl/cmake in the build image)
and applies its schema at startup instead. One ORM per project.
```

บรรทัด `Enabled in <path>: ...` ใช้ได้ฟรีอยู่แล้ว — มันพิมพ์ชื่อ feature ดิบ
ซึ่งจะเป็น `seaorm-postgres` และอ่านออกในตัว

### Conflict / undo

`slot_conflict` → `datastore_conflict` วันนี้แตกสองทาง (`document` vs Diesel)
เพิ่มทางที่สามโดย **ไม่เพิ่ม if**: ทำ `datastore_conflict` เป็น match บน
"feature ที่ครองช่องอยู่" → 3 arm

| occupant | ขั้นตอน restore ที่ต้องพิมพ์ |
|---|---|
| `sqlite`/`postgres`/`mysql` (Diesel) | คืน `diesel_db.rs` + `diesel_repo.rs` จาก `kodegen new` สด |
| `document` | คืนสาม block ใน `bootstrap.rs`; `mongo_repo.rs` ทิ้งไว้ได้ (cfg ปิดแล้ว) |
| `seaorm-*` | **ข้อความเดียวกับ document เป๊ะ** เปลี่ยนแค่ชื่อไฟล์/ชื่อ type |

สอง arm หลังใช้ template ข้อความร่วมกัน (`"put the three bootstrap blocks back"`)
ต่างกันแค่ argument — เพราะทั้งคู่คือ "สลับ block ไม่ใช่สลับไฟล์" เหมือนกัน

---

## 3. Decision: Feature graph

### เลือก: ORM เป็น gate แยกชั้น มิเรอร์ `relational` ทุกประการ

```toml
# ORM ตัวที่สอง: pure Rust ทั้งสาย (sqlx + rustls) จึงไม่มีภาษี C library
# เหมือน Diesel — runtime image distroless/cc ไม่ต้องเพิ่มอะไร และ build stage
# ไม่ต้องมี perl/cmake. ยึด "1" ไว้ด้วย MSRV: 2.0.0 ประกาศ rust-version 1.94
# ส่วน 1.1.20 ประกาศ 1.81 — ซึ่งอยู่ใต้ 1.88 ของ package นี้. default features
# ลาก chrono/bigdecimal/rust_decimal เข้ามาโดยที่ตาราง users เป็น TEXT ล้วน
# จึงปิดทิ้งทั้งชุด. `runtime-tokio` (ไม่ใช่ `-rustls`) เพราะ TLS provider ถูก
# เลือกที่ sqlx ข้างล่างแทน — ดูเหตุผลใน §3.1
sea-orm = { version = "1", default-features = false, features = [
    "macros",
    "runtime-tokio",
], optional = true }
# ประกาศตรง ๆ ทั้งที่ sea-orm ลากมันมาให้อยู่แล้ว **เพื่ออย่างเดียว**: เลือก
# crypto provider ของ rustls ให้ตรงกับ aws-sdk-s3 (§3.1). default features ของ
# sqlx (`any`/`macros`/`migrate`/`json`) เป็น compile-time query checking,
# migration framework ตัวที่สอง และ runtime backend dispatch — ไม่ใช้สักอย่าง
sqlx = { version = "0.8", default-features = false, optional = true }

[features]
# มิเรอร์ `relational`: เป็นตัวที่ infrastructure/mod.rs ใช้ gate โมดูล SeaORM
# และเป็นตัวที่ทำให้ backend ข้างล่างเปิด dep ชุดเดียวกัน. runtime + TLS อยู่
# **ตรงนี้** ไม่ใช่ต่อ backend: ลืมใส่แล้ว sqlx panic ตอน runtime ไม่ใช่ตอน build
seaorm = ["dep:sea-orm", "sqlx/tls-rustls-aws-lc-rs", "tokio/sync"]
seaorm-postgres = ["seaorm", "sea-orm/sqlx-postgres"]
seaorm-mysql = ["seaorm", "sea-orm/sqlx-mysql"]
```

### 3.1 TLS provider — จุดที่จะกลายเป็น panic ถ้าปล่อยตาม default

ตรวจ feature graph ของ sqlx 0.8.6 จริงแล้ว:
`sea-orm/runtime-tokio-rustls` → `sqlx/runtime-tokio-rustls` → `tls-rustls-ring` → **ring**
ขณะที่ `aws-sdk-s3` (feature `blob`) ใช้ `default-https-client` → rustls + **aws-lc-rs**

โปรเจกต์ที่เปิด `seaorm-postgres` + `blob` พร้อมกัน (ถูกกฎหมายเต็มที่ — คนละ slot)
จะ compile rustls 0.23 ที่มี provider สองตัว ซึ่ง **ไม่มี process default ให้เลือกอัตโนมัติ**
→ panic ตอนสร้าง TLS config ครั้งแรก **ตอน runtime ไม่ใช่ตอน build** และเฉพาะตอนที่ต่อ
ปลายทางที่เป็น TLS จริง (CI ที่ยิง compose service แบบ plain TCP จะไม่เจอ)

แก้ที่ต้นทาง: เลือก provider ให้ตรงกันเองด้วย `sqlx/tls-rustls-aws-lc-rs`
(aws-lc-rs เพราะ `blob` เป็นฝั่งที่เลือกไม่ได้) ราคาคือ dep บรรทัดเดียวที่อยู่ใน lock อยู่แล้ว

> **หมายเหตุนอกขอบเขต Phase 6:** เงื่อนไขเดียวกันน่าจะมีอยู่แล้ววันนี้ระหว่าง
> `document` (mongodb `rustls-tls`) กับ `blob` — job `generated-all` เปิดทั้งคู่และเขียว
> เพราะไม่มี endpoint ไหนเป็น TLS เลย ควรตรวจแยกเป็นงานของตัวเอง ไม่รวมเข้ามาที่นี่

- `tokio/sync` เพราะ handle ใช้ `OnceCell` ตัวเดียวกับ mongo/clickhouse
- `Slot::Datastore::features()` เพิ่มสองชื่อ → กติกา "หนึ่ง datastore ต่อโปรเจกต์"
  ครอบ Diesel↔SeaORM **ฟรี ไม่ต้องเขียนกฎใหม่**
- ไม่มี `seaorm-sqlite` — ดู §8 (ตั้งใจไม่ทำ)
- lock strategy ไม่เปลี่ยน: ประกาศ optional ครบใน template, `add` พลิก `default` อย่างเดียว

### กันไม่ให้ compile พร้อมกันโดยไม่ตั้งใจ

cargo feature เป็น **additive**: `cargo build --features seaorm-postgres` บนโปรเจกต์ที่
`default = ["sqlite"]` จะเปิดทั้งสอง ORM แล้ว **compile ผ่าน** (สอง `Db` คนละโมดูล,
`bootstrap.rs` import ตัวเดียว) — ได้ binary ที่ลาก diesel + C driver + sqlx มาทั้งคู่โดยเงียบ
กันด้วยสองบรรทัดใน `src/lib.rs` ของ template:

```rust
#[cfg(all(feature = "relational", feature = "seaorm"))]
compile_error!("one ORM per project: `relational` (Diesel) and `seaorm` cannot both be on — switch with `kodegen add <db> --orm seaorm`");
```

⚠️ **กับดักที่ต้องรู้ล่วงหน้า:** job `features` ใน CI ของ template รัน
`cargo check --locked --features <X>` ทับ default ที่เป็น `sqlite`
→ **ห้ามเพิ่มแถว `seaorm-postgres` เข้าไปใน job นั้น** มันจะชน `compile_error!`
โดยการออกแบบ (และ `--no-default-features` ก็ใช้ไม่ได้ ตามที่ plan-persistence §4 ระบุไว้)
sea-orm ยังถูก pin ใน lock อยู่ดีเพราะเป็น optional dep — สิ่งที่ job นั้นพิสูจน์คือ
"resolve ได้", ส่วน "compile ได้" ไปพิสูจน์ใน matrix ฝั่ง kodegen (§7) ซึ่งเป็นที่เดียวที่
`bootstrap.rs` ถูกเขียนใหม่ให้เข้ากับ SeaORM จริง — ใส่ comment บรรทัดนี้ไว้ใน YAML

---

## 4. Decision: Async bootstrap — **ไม่ทำ bootstrap เป็น async**

### เลือก: lazy `OnceCell` แบบ mongo

```rust
pub struct Db(Arc<Inner>);
struct Inner { url: String, conn: OnceCell<DatabaseConnection> }
impl Db {
    pub fn new(url: impl Into<String>) -> Self { ... }          // sync, ไม่ต่อสาย
    async fn conn(&self) -> Result<&DatabaseConnection, DomainError> {
        self.0.conn.get_or_try_init(|| async {
            let db = Database::connect(opts(&self.0.url)).await?;
            for stmt in SCHEMA { db.execute_unprepared(stmt).await?; }   // §5
            Ok(db)
        }).await.map_err(infra)
    }
}
```

### ชั่งกับทางเลือก "ทำ bootstrap เป็น async จริง"

ตรวจ call site จริงแล้ว **ripple ในโค้ดเล็กกว่าที่กลัว** — `main.rs`,
`tests/api.rs` (5 จุด), `examples/alloc_bench.rs` อยู่ใน async context อยู่แล้วทุกจุด
เติม `.await` แปดที่ก็จบ ราคาจริงอยู่คนละที่:

1. **มันจะทำให้ `bootstrap.rs` ทุกเวอร์ชันที่ generate ไปแล้ว "ไม่ใช่ของ kodegen" ทันที** —
   `BOOTSTRAP_USE` / `BOOTSTRAP_DB` / `BOOTSTRAP_BUILD_USER` ถูก match แบบ verbatim
   โปรเจกต์ที่ generate ก่อนหน้านี้จะโดน refuse ทุกครั้งที่ `add mongodb`/`add postgres`
   นี่คือการทำลาย compatibility เพื่อเปลี่ยน signature ของฟังก์ชันที่ผู้ใช้ไม่เคยเรียกเอง
2. **มันซื้ออะไรไม่ได้เลย** — ของ async มีอย่างเดียวคือ `Database::connect`
   และ "ต่อสายตอน request แรก" **ดีกว่า** "ต่อสายตอน boot" สำหรับ service:
   DB ที่ยังไม่ขึ้นไม่ควรกัน pod ไม่ให้ start และ `DomainError::Unavailable → 503`
   ก็มีอยู่แล้วในเทมเพลต (ข้อความนี้เขียนไว้ตรง ๆ ใน `mongo_repo.rs` แล้ว)
3. **จุดขาย "async ไม่ต้อง spawn_blocking" ไม่ได้หายไปไหน** — มันอยู่ที่ *repository*:
   ไม่มี `spawn_blocking`, ไม่มี r2d2, ไม่มี `Db::run` closure, ไม่มี blocking pool
   นั่นคือของจริงที่ผู้ใช้ได้ และมันไม่ต้องการ composition root ที่เป็น async

ข้อแลกที่รับ: connection error ครั้งแรกโผล่ตอน request แรก ไม่ใช่ตอน boot —
`get_or_try_init` ไม่ cache ความล้มเหลว จึง retry เองรอบถัดไป (พฤติกรรมเดียวกับ mongo เป๊ะ)

---

## 5. Decision: SeaORM path ไม่มี migration framework

### เลือก: idempotent DDL ตอนต่อสายครั้งแรก, ไม่ใช้ `sea-orm-migration`

`sea-orm-migration` ราคาที่ต้องจ่าย: dep เพิ่ม + pattern เป็น struct ต่อ migration
ใน Rust (+ `Migrator` list ที่ต้องมี anchor ใหม่ให้ `add` แทรก) + **มันไม่อ่าน
`migrations/*/up.sql` ที่ระบบปัจจุบันเป็นเจ้าของ** → `KNOWN_SEEDS`, การ rewrite ต่อ dialect
และ TimescaleDB ต้องถูก duplicate เป็นเวอร์ชัน Rust ทั้งชุด

ทางที่เลือกแทน — และมันคือ **precedent ที่มีอยู่แล้วสี่ที่ในโปรเจกต์นี้**:
mongo สร้าง collection ตอน write, clickhouse `CREATE TABLE IF NOT EXISTS` ตอน startup,
s3 สร้าง bucket ถ้ายังไม่มี, และ plan-persistence §3.4 เขียนไว้เองแล้วว่า MSSQL จะ
"ship schema.sql รันตอน startup แบบ idempotent" — SeaORM เข้ากลุ่มนี้ ไม่ใช่ข้อยกเว้น

```rust
// sea_db.rs
const SCHEMA: &[&str] = &[ "CREATE TABLE IF NOT EXISTS users (...)" ];
```

- `&[&str]` ไม่ใช่ string ก้อนเดียว: multi-statement ใน one round trip ไม่ได้เปิดเสมอ
  บน MySQL — สั่งทีละ statement คือสิ่งที่ทำงานเหมือนกันทุก backend
- ต้องมี `Relational::startup_ddl() -> &'static [&'static str]` **แยกจาก `up_sql()`**
  ไม่ใช่แก้ `up_sql()` ให้เป็น `IF NOT EXISTS`: `up_sql()` เป็น migration ที่มีเวอร์ชัน
  (การซ่อน conflict ในนั้นคือการซ่อน bug) และมันถูก pin ด้วย `KNOWN_SEEDS` อยู่
  → เพิ่มค่าคงที่สามตัว ไม่แตะของเดิมเลย
- TimescaleDB ต้องเขียนใหม่ให้ idempotent จริง: `CREATE EXTENSION IF NOT EXISTS` (มีแล้ว),
  `CREATE TABLE IF NOT EXISTS`, และ `create_hypertable(..., if_not_exists => TRUE)`
- `migrations/` ที่ค้างอยู่: **ไม่ลบ** (`write_plan` ไม่มี delete และการลบไฟล์ผู้ใช้ไม่ใช่
  งานของ `add`) — `add ... --orm seaorm` ข้าม `migrations()` ทั้งฟังก์ชัน แล้ว warn:
  `migrations/ is not used on the SeaORM path (the schema lives in src/infrastructure/sea_db.rs) — delete it, or wire up sea-orm-migration when your schema outgrows the seed`

**บอกผู้ใช้ตรง ๆ ว่านี่คือของที่แลกไป**: เลือก SeaORM = ไม่ได้ versioned migration
ต้องเขียนใน `--list`, ใน README ทั้งสองไฟล์ และในหัวไฟล์ `sea_db.rs`

---

## 6. Decision: อะไรถูก generate / rewrite

เทียบกับเส้นทาง Diesel ที่แตะ **หกไฟล์** — SeaORM แตะ **สี่** และไม่มีไฟล์ไหนถูกเขียนทับ:

| ไฟล์ | Diesel path วันนี้ | SeaORM path |
|---|---|---|
| `Cargo.toml` | พลิก `default` | เหมือนกัน (`seaorm-postgres`) |
| `src/infrastructure/diesel_db.rs` | **เขียนทับทั้งไฟล์** | ไม่แตะ — cfg ปิดไปพร้อม `relational` |
| `src/infrastructure/diesel_repo.rs` | patch upsert + test module | ไม่แตะ — เหตุผลเดียวกัน |
| `src/infrastructure/sea_db.rs` | — | **สร้างใหม่**: `Db` + `OnceCell` + `ConnectOptions` + `SCHEMA` + `infra()` |
| `src/infrastructure/sea_repo.rs` | — | **สร้างใหม่**: entity + `SeaUserRepository` + env-gated tests |
| `src/infrastructure/mod.rs` | — | append `#[cfg(feature = "seaorm")] pub mod ...` ×2 |
| `src/bootstrap.rs` | ไม่แตะ | **สลับสาม block** (verbatim หรือ refuse) |
| `migrations/*/up.sql` | เขียนตาม dialect | ข้าม + warn (§5) |
| `Dockerfile` | เพิ่ม perl/make/cmake | **ไม่แตะ** ← payoff ที่ผู้ใช้เห็นจริง |
| `docker-compose.dev.yml` | append service | เหมือนกัน — **ใช้ `POSTGRES_COMPOSE`/`MYSQL_COMPOSE` เดิม ไม่มี const ใหม่** |
| env | `DATABASE_URL` | **เหมือนกันทุกตัวอักษร** — sea-orm รับ `postgres://`/`mysql://` |

### `sea_repo.rs` — สิ่งที่ต้องระวัง

```rust
// entity อยู่ไฟล์เดียวกับ repo — สมมาตรกับ diesel::table! ที่อยู่ใน diesel_repo.rs
#[derive(DeriveEntityModel)] ... struct Model { #[sea_orm(primary_key, auto_increment = false)] id: String, name: String }

// save = upsert ตามสัญญาของ port
Entity::insert(active)
    .on_conflict(OnConflict::column(Column::Id).update_column(Column::Name).to_owned())
    .exec(conn).await
```

- **ห้ามใช้ `on_conflict_do_nothing`** — port ระบุว่า `save` ทับของเดิม ไม่ใช่เมิน
  (และ MySQL ไม่มี `DO NOTHING` จริง sea-orm ต้อง polyfill เป็น `SET id = id`)
- บน MySQL `OnConflict` render เป็น `ON DUPLICATE KEY UPDATE` ซึ่ง **ไม่สนใจ conflict target**
  (มันดูจาก unique index ตัวไหนก็ได้) — ต่างจาก Diesel path ที่ต้องส่ง
  `diesel::dsl::DuplicatedKeys` เอง จึงใช้โค้ดชุดเดียวกันได้ทั้งสอง backend
  ⇒ **ไม่มี `conflict_target()` เวอร์ชัน SeaORM**
- ⚠️ **กับดักที่ต้องออกแบบเผื่อไว้: `DbErr::RecordNotInserted`** — sea-orm คืน `Err` ตัวนี้
  เมื่อ insert/update แล้วได้ 0 row และ **MySQL คืน 0 affected row เมื่อค่าใหม่เท่าค่าเดิม**
  ⇒ `save(User("u1","Alice"))` ซ้ำด้วยค่าเดิมจะกลายเป็น error บน MySQL แต่ผ่านบน PostgreSQL
  ซึ่ง **ผิดสัญญาของ port ตรง ๆ** ("saving an existing id overwrites it and returns Ok")
  ทางแก้ที่ต้องลองตามลำดับ: `exec_without_returning()` (คืน `u64` แทนที่จะ error ที่ 0 row)
  ก่อน แล้วค่อยพิจารณาดัก `RecordNotInserted` แล้ว map เป็น `Ok(())`
  → **นี่คือเหตุผลหลักที่ 6b ต้องมี docker gate ของตัวเอง** และ test เดิม
  (`save_then_get_roundtrips` เขียน `("u1","Alice")` ทุกครั้ง) เจอเคสนี้พอดีตั้งแต่รันรอบที่สอง
- `infra()` map `DbErr` ที่เป็น connection/acquire → `Unavailable` (503), ที่เหลือ → `Other`
  — สมมาตรกับ `mongo_repo::infra` ที่ทำแบบเดียวกันกับ `ErrorKind::Io`/`ServerSelection`
- test module = รูปแบบเดียวกับ `REPO_TESTS_TPL` เป๊ะ: gate ด้วย `DATABASE_URL`,
  ไม่ใช้ `#[ignore]`, ไม่มี server ก็เขียว

### `rewrite_bootstrap` — generalize ไม่ใช่ copy

วันนี้เป็นฟังก์ชันเฉพาะ mongo ที่ hard-code สาม block + guard `if out.contains("Db")`
ทำให้รับ target เป็นพารามิเตอร์ (สาม block + guard + ข้อความ refusal) → สองผู้เรียก

| target | สาม block | guard หลังสลับ |
|---|---|---|
| Mongo | `Mongo`, `MongoUserRepository` | ห้ามเหลือ `Db` (type หายไปจริง) |
| SeaORM | `Db`, `SeaUserRepository` | **ห้ามเหลือ `diesel`/`Diesel`** (type ชื่อเดิม แต่ API คนละตัว: ใครเรียก `db.run(...)` เองจะพัง) |

guard ของ SeaORM ต่างจาก mongo เพราะ type ชื่อ `Db` เหมือนเดิม — ซึ่งเป็นความจงใจ ดู §7

---

## 7. Decision: scaffold state detection — **ไม่เพิ่ม state ที่สาม**

`scaffold.rs` วันนี้มีบรรทัดเดียว:
`let db_ty = if c.contains("Option<&Mongo>") { "Mongo" } else { "Db" };`

ทางที่เลือก: **ตั้งชื่อ handle ของ SeaORM ว่า `Db` เหมือนกัน** →
`scaffold.rs` **ไม่ต้องแก้แม้แต่บรรทัดเดียว**, builder ที่ `kodegen feature` เคย scaffold ไว้
(`_db: Option<&Db>`) ยัง compile ผ่านหลัง `add --orm seaorm`, และ `FEATURE_BUILDER_DB`
ไม่ต้องมีคู่ของ SeaORM ปลอดภัยเพราะ `diesel_db::Db` กับ `sea_db::Db` อยู่คนละ cfg
ไม่มีทาง compile พร้อมกัน (§3)

**กติกาที่เขียนไว้เลยสำหรับ handle ตัวถัดไป:** ตั้งชื่อ `Db` เสมอ
`Mongo` คือข้อยกเว้นเชิงประวัติศาสตร์ตัวเดียว — ถ้าวันหนึ่งมี state ที่สี่จริง
ทางแก้คือ **rename `Mongo` → `Db` แล้วลบ branch ทิ้ง** ไม่ใช่เพิ่ม branch ที่สาม
(การเปลี่ยนนั้นแตะ `add mongodb` ซึ่ง ship ไปแล้ว จึงไม่อยู่ใน Phase 6)

---

## 8. Phasing

| Phase | ขอบเขต | ขนาด | เกณฑ์จบ (ship ได้เอง) |
|---|---|---|---|
| **6a** | แกน ORM ทั้งเส้น + PostgreSQL/TimescaleDB: `Orm` enum + `--orm` ใน `add`/`new`, feature graph + `compile_error!` guard, `sea_db.rs` + `sea_repo.rs`, generalize `rewrite_bootstrap`, `datastore_conflict` 3 arm, `--list`, unit tests, **หนึ่งแถว CI ที่รันกับ Postgres จริง**, README ×2 | **M** | `add postgres --orm seaorm` → generated project ผ่าน fmt/clippy/`test --locked`; ไม่ตั้ง `DATABASE_URL` ยังได้ in-memory; ตั้งแล้วต่อ Postgres จริง สร้าง schema เอง upsert ได้; Dockerfile ไม่ถูกแตะ; lock ไม่เปลี่ยนตอนพลิก feature |
| **6b** | MySQL/MariaDB บน SeaORM: `seaorm-mysql`, หนึ่งแถว CI ที่รันกับ MySQL จริง, (ทางเลือก) ย้าย datastore ของ job `generated-all` | **S** | `add mysql --orm seaorm` ผ่านเกณฑ์เดียวกัน **และพิสูจน์ `ON DUPLICATE KEY UPDATE` ด้วยการ save ซ้ำ id เดิมกับ server จริง** |

**ทำไมแบ่งตรงนี้:** เส้นแบ่งคือ *สิ่งที่พิสูจน์ใหม่* ไม่ใช่จำนวนบรรทัด — 6a พิสูจน์ว่าแกน ORM
ทั้งเส้นทำงาน (feature graph, bootstrap swap, lazy connect, startup DDL, lock)
6b พิสูจน์อย่างเดียวที่ 6a พิสูจน์แทนไม่ได้: upsert ของ MySQL ซึ่งเป็น runtime failure
ไม่ใช่ compile failure (บทเรียนเดียวกับ `TEXT PRIMARY KEY` ในแผนเดิม)
ถ้า 6b ไม่ได้ทำต่อ 6a ก็ยังเป็นของที่ใช้ได้จริงและ document ได้ครบ

ประเมินบรรทัด: 6a ≈ `capability.rs` +400–500 (สองในสามเป็น template constant),
`main.rs` +20, template `Cargo.toml` +14, `lib.rs` +3, CI +12 · 6b ≈ +80 รวมทุกไฟล์

---

## 9. Decision: Testing

หลักเดิม: **capability × default + จุดที่พิสูจน์อะไรใหม่เท่านั้น** ไม่ทำ cross product

### 9.1 unit test ฝั่ง kodegen (เร็ว รันทุกครั้ง)

ต่อยอด fixture เดิมใน `capability.rs::tests` ทั้งหมด ไม่มีกลไกใหม่:

- `--orm seaorm` บน capability ที่ไม่ใช่ relational → error (ก่อนแตะไฟล์)
- `enable_feature` เปลี่ยน `["sqlite"]` → `["seaorm-postgres"]` และ **ไม่แตะ `[dependencies]`**
- `add postgres` (diesel) บนโปรเจกต์ที่เป็น `seaorm-postgres` → refuse พร้อมสามขั้นตอน (และกลับกัน)
- `bootstrap.rs` ที่ถูกแก้มือ → refuse ไม่สลับครึ่งเดียว
- หลัง `add --orm seaorm` แล้ว `feature notification` ยัง scaffold ได้ และได้ `Option<&Db>`
- `--dry-run` ไม่เขียนไฟล์ และ plan ตรงกับ real run (pattern เดิม)
- test ที่ pin `Slot::features()` ↔ `Capability::feature()` ต้องวนทั้งสอง ORM

### 9.2 CI ฝั่ง kodegen — เพิ่ม **หนึ่งแถวต่อ phase** ไม่มี job ใหม่

job `generated` ปัจจุบันเป็น matrix ของชื่อ capability → เปลี่ยนเป็นมี `include:`
สองแถวที่พก key `orm: seaorm` เพิ่ม (แถวเดิมไม่ต้องแก้ ใช้ `${{ matrix.orm && ... || '' }}`)

| แถวใหม่ | phase | พิสูจน์อะไรที่แถวอื่นพิสูจน์ไม่ได้ |
|---|---|---|
| `postgres` + `--orm seaorm` | 6a | โค้ด SeaORM ทั้งชุดถูก compile เป็นครั้งแรก + สถานะที่ Diesel ถูก cfg ปิดทั้ง `bootstrap.rs` และ `mod.rs` |
| `mysql` + `--orm seaorm` | 6b | `ON DUPLICATE KEY UPDATE` ของ sea-orm กับ server จริง |

**docker gate: จำเป็นทั้งสองแถว และเป็นครั้งแรกที่ทำได้** — job `generated-all`
เขียน comment ไว้เองว่าใช้ mongodb เพราะ *"Diesel needs its migration applied before a
query works and there is no step here that applies one"* — เส้นทาง SeaORM
apply schema ของตัวเองตอนต่อสาย จึงเป็น datastore ตัวแรกที่รันสดในแถว matrix ได้
วิธี boot ใช้ trick เดิมของแถว `influxdb` เป๊ะ: `docker compose -f docker-compose.dev.yml
up -d --wait` จากไฟล์ที่ `add` เพิ่งเขียน แล้ว export `DATABASE_URL` เข้า `$GITHUB_ENV`
เฉพาะแถวนั้น (ได้ตรวจ compose file ที่ generate ไปในตัวด้วย)

### 9.3 ช่องว่างที่รู้ตัว

`generated-all` (job ที่พิสูจน์ feature unification) ใช้ mongodb เป็น datastore
⇒ **sqlx จะไม่เคยถูก compile พร้อม aws-sdk-s3/redis/clickhouse** — และไม่มี job ไหนทำได้
เพราะ mongo กับ seaorm อยู่ช่องเดียวกัน (ข้อจำกัดนี้มีอยู่แล้ววันนี้ระหว่าง postgres กับ mongo)

ทางปิด (จัดไว้ที่ 6b เป็น **ทางเลือก** ตัดออกได้โดยไม่กระทบเกณฑ์จบ):
สลับ datastore ของ `generated-all` เป็น `seaorm-postgres` แล้วย้าย live test ของ mongo
ไปอยู่ในแถว matrix ของตัวเอง (boot จาก compose ที่ `add mongodb` เขียน — trick เดียวกับ influxdb)
ได้สามอย่างพร้อมกัน: sqlx เข้า unification job, datastore ถูกทดสอบสดใน `generated-all`,
และ mongo ยังมี live test เหมือนเดิม ราคา ~10 บรรทัด YAML

### 9.4 CI ของ template

**ไม่แตะ** — job `features` ห้ามมีแถว `seaorm-*` (§3) และ job `audit` ครอบ dep ใหม่ให้ฟรี
job lock-refresh รายสัปดาห์ที่ plan-persistence §6.1 บันทึกไว้ **สำคัญขึ้นมาก** หลัง phase นี้:
sqlx/sea-query/rustls คือ dependency tree ที่ advisory ขยับบ่อยที่สุดในกราฟทั้งหมด

---

## 10. สิ่งที่จะ *ไม่* ทำ (ตั้งใจ)

- ❌ **`seaorm-sqlite`** — ไม่มี `Capability::Sqlite` ให้เรียกตั้งแต่แรก และเส้นทาง
  "เขียวโดยไม่มี server" คือ `in_memory_repo` ไม่ใช่ SQLite ⇒ feature นี้จะเป็นน้ำหนักตาย
  แถม `sqlx-sqlite` ลาก `libsqlite3-sys{bundled}` + `cc` กลับเข้ามา = ทิ้งจุดขาย
  "ไม่ต้องมี C toolchain" ของทั้ง phase ไปเปล่า ๆ วันที่มีคนขอ = เพิ่มสามบรรทัด
- ❌ **`sea-orm-migration` / versioned migration บนเส้นทาง SeaORM** (§5) — ประกาศเป็น
  ข้อแลกที่ผู้ใช้เห็นก่อนเลือก ไม่ใช่ของที่ลืม. เพิ่มเติม: crate นี้ประกาศ
  `rust-version = 1.85` ทั้งที่ 2.0 ของมันพึ่ง sea-orm 2.0 ที่ต้องการ 1.94 ⇒ MSRV-aware
  resolution จะเลือก 2.0 มาให้บน workspace 1.88 แล้วได้ lock ที่ build ไม่ได้
  (ต้อง pin `"1.1"` เอง) — อีกหนึ่งเหตุผลที่ไม่แตะ
- ❌ **`sea-orm-cli` / entity generation จาก DB จริง** — เป็น workflow ของโปรเจกต์ผู้ใช้ ไม่ใช่ของ generator
- ❌ **ทำ `build_services_from_env()` เป็น async** (§4)
- ❌ **trait `Orm` ครอบ Diesel/SeaORM** — `UserRepository` คือ abstraction ที่มีอยู่แล้ว
  (ข้อนี้อยู่ใน "ไม่ทำ" ของแผนเดิมและยังจริงกว่าเดิมหลังเห็นโค้ด)
- ❌ **sea-orm 2.0** — ต้องการ rustc 1.94 ขึ้นตอนที่ template ขึ้น MSRV เท่านั้น
- ❌ **`--orm` บน capability ที่ไม่ใช่ relational** — error ไม่ใช่ ignore
- ❌ **relational สองตัว / Diesel+SeaORM พร้อมกัน** — slot + `compile_error!`
- ❌ **CI matrix แบบ ORM × DB เต็มตาราง** — สองแถว จบ
- ❌ **แตะ `scaffold.rs`** (§7)

---

## 11. ความเสี่ยงที่เหลือ / ต้องเฝ้า

1. **upsert ของ MySQL + `DbErr::RecordNotInserted`** (§6) — save ซ้ำด้วยค่าเดิมได้
   0 affected row บน MySQL ซึ่ง sea-orm ตีเป็น error ⇒ ผิดสัญญาของ port
   เป็น runtime behavior ล้วน ปิดด้วย docker gate ของ 6b เท่านั้น
   **ห้าม ship 6b โดยที่แถวนั้นไม่เขียว และห้ามเชื่อว่า PostgreSQL เขียวแล้วแปลว่า MySQL เขียว**
2. **rustls crypto provider ชนกัน** (§3.1) — ปิดด้วย `sqlx/tls-rustls-aws-lc-rs` ตั้งแต่แรก
   ไม่ใช่รอ CI จับ เพราะ CI ทั้งหมดยิงไปที่ compose service แบบ plain TCP จึงจับไม่ได้อยู่ดี
   ตรวจ `cargo tree -e features -i rustls` ของโปรเจกต์ที่ generate ด้วย `--with s3` หนึ่งครั้ง
   ตอนทำ 6a คือวิธียืนยันที่ถูกที่สุด
3. **DDL ของ TimescaleDB ต้อง idempotent จริง** — `create_hypertable` ที่ลืม
   `if_not_exists => TRUE` จะพังตอน restart ครั้งที่สอง ไม่ใช่ครั้งแรก และ CI
   ไม่มีแถว timescale ที่รันสด (เหมือนเส้นทาง Diesel วันนี้) → ต้องลองมือครั้งเดียวก่อน merge
4. **sea-orm 1.x เข้าสู่โหมดประคอง** หลัง 2.0 ออก — security fix อาจมาที่ 2.x ก่อน
   ทางออกคือ MSRV 1.94 ซึ่งเป็นเรื่องเดียวกับที่ aws-sdk-s3 รออยู่ เฝ้าผ่าน job `audit`
5. **lock โต 406 → ~470** และ `sqlx` เป็น compile time ก้อนใหญ่ก้อนใหม่ในแถว CI นั้น
6. **`compile_error!` guard กับ job `features` ของ template** — ถ้ามีคนเผลอเพิ่มแถว
   `seaorm-postgres` เข้าไป CI จะแดงแบบที่อ่านไม่ออกว่าเป็นเจตนา → comment ใน YAML คือ
   สิ่งเดียวที่กัน (§3)
7. **`migrations/` ที่ค้างบนโปรเจกต์ SeaORM** — warn อย่างเดียว ไม่ลบ; ถ้ามีคนกลับไป
   `--orm diesel` ทีหลัง ไฟล์ยังอยู่ครบและใช้ได้ ซึ่งเป็นผลข้างเคียงที่ดี

---

## 12. CTO revisions (approve-with-changes) + สิ่งที่พบตอน implement 6a

บันทึกไว้ในแผนตามคำสั่ง: สี่ข้อแรกคือ amendment ที่ CTO สั่งก่อนเริ่ม, ที่เหลือคือ
จุดที่ **แผนขัดกับความจริง** ตอนรันจริง (ทุกข้อมีผลรันเป็นหลักฐาน ไม่ใช่การคาดเดา)

### 12.1 CTO amendments (บังคับ — implement ครบทั้งสี่)

| # | สั่งอะไร | ทำที่ไหน |
|---|---|---|
| 1 | `exec_without_returning()` ตั้งแต่แรก + test `save_twice_identical_values_is_ok` | `SEA_REPO_TPL` — save ใช้ `exec_without_returning`, test เขียนค่าเดิมสองครั้งใน test เดียว (แยกสอง test แล้วแถวเก่าจาก run ก่อนจะทำให้ผ่านด้วยเหตุผลผิด) |
| 2 | CI แถว seaorm รัน `cargo test` สองรอบทับ DB เดิม | `.github/workflows/ci.yml` — step "Test" + "Test again over the same database" (`if: matrix.orm == 'seaorm'`), compose ค้างไว้ระหว่างสองรอบ |
| 3 | DDL privilege failure mode ใน README + หัวไฟล์ `sea_db.rs` | ทำแล้ว **แต่ข้อเท็จจริงไม่ตรงกับที่สั่ง** — ดู §12.2 |
| 4 | guard "ห้ามเหลือ diesel/Diesel" ต้องมี positive-path test บน template สด | `the_seaorm_guard_passes_on_a_fresh_bootstrap_and_refuses_a_spiked_one` — เรียก `rewrite_bootstrap` กับ `bootstrap.rs` ของ template ตรง ๆ แล้ว assert ว่าไม่เหลือคำทั้งสอง (คือเหตุผลที่ BOOTSTRAP_*_SEA ทั้งสามก้อน **ห้ามมีคำว่า Diesel ใน comment** — มี comment เตือนไว้ที่ตัว const แล้ว) |
| 5 | README idempotent-DDL: เส้นแบ่งรูปธรรม + pin `sea-orm-migration = "1.1"` + แยก `Db` สอง semantics | README ทั้งสองไฟล์ + หัว `sea_db.rs`: "การแก้ schema ครั้งแรกบน DB ที่มีข้อมูล production แล้ว" คือเส้นแบ่ง, และ `use crate::infrastructure::sea_db::Db;` คือวิธีระบุว่าหมายถึงตัวไหน |

### 12.2 แผน/คำสั่งขัดกับความจริง (วัดแล้ว)

1. **DDL privilege — ทางออกที่ CTO เขียนไว้ใช้ไม่ได้จริงบน PostgreSQL.**
   คำสั่งเดิม: "app role ไม่มีสิทธิ์ DDL → 503 วนตอน request แรก … ทางออก: รัน SCHEMA
   มือด้วย role ที่มีสิทธิ์" — ผลวัดจริง (role ที่ถูก `REVOKE CREATE ON SCHEMA public`):
   - เป็น **500 ไม่ใช่ 503** — server ตอบแล้ว (`DbErr::Exec`), ตาม mapping เดียวกับ
     `mongo_repo::infra` ที่แยก "ต่อไม่ติด" ออกจาก "server ปฏิเสธ"
   - **การรัน SCHEMA มือไว้ก่อนไม่ช่วย**: PostgreSQL เช็ค `CREATE` บน schema **ก่อน**
     เช็คว่า object มีอยู่แล้ว ⇒ `CREATE TABLE IF NOT EXISTS users` ยัง error
     `permission denied for schema public` แม้ตาราง `users` มีอยู่แล้ว (ทดสอบตรง ๆ ด้วย psql)
   - ทางออกจริงมีสองทาง เขียนลง README/หัวไฟล์แล้ว: `GRANT CREATE ON SCHEMA public TO <role>`
     หรือ **ล้าง `SCHEMA` ให้ว่าง** แล้วจัดการ schema นอกแอป (= การย้ายไป migration นั่นเอง)

2. **`CREATE TABLE IF NOT EXISTS` idempotent แต่ไม่ atomic — แผนไม่ได้พูดถึง.**
   docker gate รอบแรกบน DB ว่างล้ม: สาม `#[tokio::test]` ต่อสายพร้อมกัน แล้ว PostgreSQL
   ตอบ `23505 duplicate key value violates unique constraint "pg_type_typname_nsp_index"`
   ⇒ เป็น race จริงที่เกิดได้กับ **หลาย pod ที่ boot พร้อมกันบน DB ว่าง** ไม่ใช่แค่ใน test
   แก้ที่ต้นเหตุด้วยสามบรรทัดใน `conn()`: statement ไหน error ให้ลองซ้ำ **หนึ่งครั้ง**
   (รอบสองมันเป็น no-op แล้ว; error รอบสองคือของจริงและถูก return) — ไม่ใช่แก้เฉพาะ test

3. **`repo()` ใน test ห้าม share handle ข้าม test (ตรงข้ามกับ `REPO_TESTS_TPL` ของ Diesel).**
   ลองทำแบบ Diesel (`OnceLock` ตัวเดียวทั้ง binary) แล้วพัง: `#[tokio::test]` สร้าง runtime
   ต่อ test และ drop ทิ้งท้าย test — sqlx pool ผูกกับ runtime ที่สร้างมัน ⇒ test ที่สอง
   รอ acquire จนครบ 30 วิแล้วได้ `Unavailable` จึงกลับไปเป็น handle ต่อ test
   และให้ retry ในข้อ 2 เป็นตัวรับ race แทน (เขียนเหตุผลไว้ในหัว `fn repo()` แล้ว)

4. **CI matrix: `include:` ที่มี `capability: postgres` ซ้ำกับแถวเดิมจะถูก *merge* เข้าแถวเดิม**
   ตามกติกาของ GitHub Actions (ไม่เกิดแถวใหม่ และแถว postgres เดิมจะกลายเป็น seaorm)
   ⇒ ไม่สามารถทำตามแผน §9.2 ("แถวเดิมไม่ต้องแก้ + include สองแถว") ตรง ๆ ได้
   ทางที่ใช้: ทำ matrix เป็น `include:` ล้วน แถวละ job (แถวเดิมเนื้อหาไม่เปลี่ยน)

5. **`Slot::Datastore::features()` ต้องมี `seaorm-mysql` ตั้งแต่ 6a** ทั้งที่ยังไม่มี capability
   ที่ generate มันได้ — เพราะ `Capability::feature(Orm::Seaorm)` ต้อง total และ test ที่ pin
   `Slot::features()` ↔ `feature()` วนทั้งสอง ORM. **แต่ฝั่ง template ประกาศไม่ได้** (ข้อ 8)
   ⇒ 6b = `capability.rs` + template `Cargo.toml` หนึ่งบรรทัด + regen lock + CI แถวเดียว

6. **lock: 406 → 453** (แผนเดา 465–480; เคยขึ้นถึง 467 ก่อนถอด `seaorm-mysql` ตามข้อ 8),
   sea-orm 1.1.20 / sqlx 0.8.6 / sea-query 0.32.7. ไม่มี package ไหนถูกถอดจากของเดิม
   และ `--locked` ผ่านทุกคำสั่งบนโปรเจกต์ที่ generate

7. **นอกแผน (คำสั่งเพิ่มจาก coordinator ระหว่างทำ 6a): `mongodb` ย้ายไป `rustls-tls-aws-lc`.**
   เหตุผลที่ถูกต้องของการเลือก provider เดียวไม่ใช่ "client จะ panic ตอนต่อ TLS" —
   mongodb/sqlx/aws-smithy ใช้ `builder_with_provider` แบบระบุเองทุกตัว. ของจริงคือ
   สอง provider ในกราฟ ⇒ `CryptoProvider::from_crate_features()` คืน `None` ⇒ โค้ด rustls
   ที่ **ผู้ใช้เขียนเอง** แบบ `ClientConfig::builder()` จะ panic และ binary แบกสอง stack ฟรี
   (comment ใน `Cargo.toml` เขียนตามนี้แล้ว). ผลวัดหลังแก้: `cargo tree -e features -i rustls`
   เหลือ provider เดียว (`aws-lc-rs`) ทั้งบนโปรเจกต์ `--orm seaorm --with s3` และ
   `--with mongodb,s3`, `cargo tree -i ring` ว่างทั้งคู่, lock ขยับบรรทัดเดียว (`ring` หลุด
   จาก dependency list ของ rustls; ตัว package ยัง pin ผ่าน hickory-proto)

8. **`seaorm-mysql` ประกาศใน template ไม่ได้ตั้งแต่ 6a — RUSTSEC-2023-0071 (พบตอน review).**
   `sea-orm/sqlx-mysql` ลาก `rsa` เข้า `Cargo.lock` และ `rsa` **ยังไม่มีเวอร์ชันที่แก้**
   (Marvin attack, timing sidechannel). `cargo audit` อ่าน lock ล้วน ๆ ไม่สนว่า feature ไหนเปิด
   ⇒ แค่ประกาศ feature ทิ้งไว้ก็ทำให้ job `audit` ของ **ทุกโปรเจกต์ที่ generate** แดงตั้งแต่ commit แรก
   วัดจริงบน lock ของ template: ก่อนถอด 3 advisories (`bytes`, `time`, `rsa`) — หลังถอดเหลือ 2
   ซึ่งเท่ากับ HEAD เดิมพอดี (`bytes` RUSTSEC-2026-0007, `time` RUSTSEC-2026-0009 — ทั้งคู่มี fix
   และมีมาก่อน 6a, อยู่นอกขอบเขตงานนี้). ถอดออก 14 package (`rsa`, `sqlx-mysql`, `pkcs1/8`,
   `num-bigint-dig`, `der`, `spki`, …) ไม่มี package ไหนถูกเพิ่มหรือขยับเวอร์ชัน
   - ฝั่ง Rust คงไว้ทั้งหมด (`feature(Orm::Seaorm)` ของ mysql/mariadb, `Slot::features()`)
     เพราะ `check_orm` ปฏิเสธก่อนอยู่แล้ว — ไม่มี code path ไหนเขียนชื่อ feature นี้ลงโปรเจกต์
   - มี test pin กันเติมกลับโดยไม่ตั้งใจ (`embedded_template_still_matches_every_signature_add_checks`)
   - **เงื่อนไขเพิ่มของ 6b:** ต้องตอบเรื่อง `rsa` ให้ได้ก่อน (รอ fix, `--ignore RUSTSEC-2023-0071`
     ใน audit job, หรือ `sqlx-mysql` ที่ไม่ต้องใช้ `rsa` สำหรับ `caching_sha2_password`)
     ไม่ใช่แค่ "เพิ่ม CI แถวที่รันกับ MySQL จริง" — **ตอบแล้วใน §13.1**

9. **retry ใน `conn()` ต้องไม่ครอบ error ที่ต่อไม่ติด (พบตอน review).** ของเดิม `is_err()` ล้วน
   ⇒ DB ที่ล่มจ่าย acquire timeout **สองรอบ** (30+30 วิ) ขัดกับที่ README/หัวไฟล์เขียนไว้ว่า 30
   และสิทธิ์ไม่พอก็ยิงซ้ำฟรี ๆ. ตอนนี้ `DbErr::Conn(_) | DbErr::ConnectionAcquire(_)` return ทันที
   ที่เหลือ (= server ตอบมาเอง รวมถึง 23505 ของข้อ 2) retry หนึ่งครั้งตามเดิม

---

## 13. 6b — สิ่งที่ทำจริงและผลวัด (ปิด Phase 6)

ขอบเขตที่ ship: `kodegen add mysql|mariadb --orm seaorm`, template ประกาศ
`seaorm-mysql`, CI แถว `mysql --orm seaorm` ที่รันกับ MySQL จริงสองรอบ,
และ housekeeping ของ lock ที่ 6a ค้างไว้

### 13.1 `rsa` / RUSTSEC-2023-0071 — ตัดสินว่า **ignore พร้อมเหตุผล** (precedent Phase 1a)

ทางเลือกที่มี: (ก) รอ fix — advisory ไม่มี patched version เลย จึงเท่ากับไม่ทำ 6b
(ข) เลิกทำ MySQL บน SeaORM (ค) ignore ใน `.cargo/audit.toml` ที่ template ship อยู่แล้ว
เลือก (ค) และเขียนเหตุผลสามข้อไว้ในไฟล์:

1. อยู่ใน lock ไม่ได้อยู่ใน build เว้นแต่ `seaorm-mysql` เป็น feature ที่เลือก
2. **ตรวจจาก source ของ `sqlx-mysql` 0.8.6 ที่ lock pin ไว้จริง ไม่ใช่จากความจำ** —
   `src/connection/auth.rs::encrypt_rsa` เป็นที่เดียวที่ใช้ crate นี้ และใช้แค่
   `RsaPublicKey::encrypt` (OAEP/SHA-1) กับ public key ที่ server ส่งมาระหว่าง
   `caching_sha2_password`/`sha256_password` — แถม `if stream.is_tls { return ... }`
   ข้ามไปเลยบน TLS. ไม่มี private key ฝั่ง client ⇒ ไม่มี decryption ให้จับเวลา
   ซึ่งเป็นสิ่งที่ Marvin attack ต้องการ ⇒ **ข้อเท็จจริง (ข) ในคำสั่งเป็นจริง**
3. ไม่มีอะไรให้ upgrade ไป
   
วัดจริงบน template lock: ไม่มี ignore = `error: 1 vulnerability found!` (rsa 0.9.10,
"No fixed upgrade is available!") · มี ignore = 0. ผลนี้ตกไปถึงทุกโปรเจกต์ที่ generate
เพราะ `.cargo/audit.toml` เป็นไฟล์ของ template. มี unit test ผูกสองอย่างนี้ไว้ด้วยกัน
(`embedded_template_still_matches_every_signature_add_checks`): ประกาศ `seaorm-mysql`
แล้วไม่มี ignore = test แดง

### 13.2 lock

- `seaorm-mysql` คืนมา 14 package เดิมเป๊ะ (`rsa`, `sqlx-mysql`, `pkcs1/8`, `der`, `spki`,
  `num-bigint-dig`, …) ไม่มี package ไหนถูกถอดหรือขยับเวอร์ชัน — diff ที่เหลือคือ
  `spin` / `const-oid` ถูกเขียนเป็น `spin 0.10.1` เพราะมีสองเวอร์ชันในกราฟแล้ว
- housekeeping ที่ 6a flag ไว้: `cargo update -p bytes -p time` → `bytes` 1.10.1→1.12.1,
  `time` 0.3.44→0.3.54 (+ `num-conv`, `time-core`, `time-macros` ตามมา)
  ⇒ **453 → 467 package, `cargo audit` เหลือ 0 unignored advisories** (จากเดิม 2)
- template ผ่าน `cargo test --locked` + `cargo clippy --locked --all-targets -D warnings`
  หลัง bump

### 13.3 ที่ไม่ต้องแก้เลย (ตามที่ §12.5 คาดไว้)

`Relational::{feature, startup_ddl, compose_service, database_url}` แยก dialect
มาตั้งแต่ 6a ทั้งหมด ⇒ 6b ฝั่ง Rust คือ **ลบ arm ปฏิเสธใน `check_orm` + สองบรรทัดใน `--list`**
`SEA_DB_TPL`/`SEA_REPO_TPL`/`SEA_SWAP`/`add_seaorm` ไม่ถูกแตะแม้แต่บรรทัดเดียว
`check_orm` ที่เหลือจึงเป็น "capability นี้มี ORM ไหม" ล้วน — ไม่ใช่ match สอง arm ที่ตอบ
`Ok` เหมือนกันทั้งคู่

### 13.4 docker gate (รันจริงบนเครื่อง ไม่ใช่แค่ CI)

| ที่รัน | ผล |
|---|---|
| `mysql:8.4` จาก compose ที่ `add` เขียน, `cargo test --locked` **สองรอบทับ DB เดิม** | เขียวทั้งสองรอบ (12 unit + 5 api) — DDL idempotent จริง |
| `save_twice_identical_values_is_ok` บน MySQL จริง | ผ่าน — `exec_without_returning` ทำงานตามที่ §12.1 สั่ง |
| `SHOW CREATE TABLE users` | `id varchar(64) NOT NULL, PRIMARY KEY (id)` — dialect lesson ของ `TEXT PRIMARY KEY` ถูกกันไว้ใน startup DDL ด้วย |
| `cargo run` + HTTP: POST ใหม่ → 201, POST id เดิมค่าใหม่ → 201 + GET เห็นค่าใหม่, **POST id เดิมค่าเดิมซ้ำสองครั้ง → 201 ทั้งคู่**, GET ที่ไม่มี → 404 | ผ่าน — เคส 0 affected rows ตรง ๆ ไม่กลายเป็น 500 |
| `mariadb:11` หนึ่งรอบ (fmt / clippy -D warnings / test สองรอบ / audit) | เขียวทั้งหมด |

โปรเจกต์ที่ generate: `--locked` ผ่านทุกคำสั่ง, lock ไม่ขยับตอนพลิก feature,
`diff` ระหว่าง mysql กับ mariadb = ชื่อ project + compose service + ชื่อ dialect ใน doc comment
เท่านั้น ⇒ CI แถว mariadb ยังไม่ต้องมีตามธรรมเนียมเดิม

### 13.5 ช่องว่างของ §9.3 — ปิดแล้ว ด้วย step เดียว ไม่ใช่การสลับ datastore

§9.3 เสนอให้สลับ datastore ของ job `generated-all` ไปเป็น `seaorm-postgres` แล้วย้าย
live test ของ mongo ไปแถวของตัวเอง — ราคาคือเสีย live-Mongo coverage ในjob นั้นไป
ทำถูกกว่า: บนแถว matrix ที่เป็น seaorm อยู่แล้ว เพิ่ม `add s3` ต่อท้ายก่อน Clippy
(`s3` เป็น additive ไม่แย่ง slot กับใคร) ⇒ `sqlx` กับ `aws-sdk-s3` ถูก compile
เข้าไบนารีเดียวกันทุก CI run ซึ่งเป็นสิ่งเดียวที่ §9.3 ต้องการจริง ๆ
(คำถาม rustls provider: `blob` เลือก aws-lc-rs ไม่ได้ ส่วน `seaorm` ถูก pin ตามมัน)
`generated-all` ไม่ต้องแตะ และ mongo ยัง live เหมือนเดิม — ราคา 3 บรรทัด YAML

พิสูจน์ก่อนเข้า CI: `new` → `add mysql --orm seaorm` → `add s3` บนโปรเจกต์เดียว
แล้ว `cargo check --locked --all-targets` ผ่าน, lock ไม่ขยับ,
`cargo tree -e features -i rustls` เหลือ provider เดียว (aws-lc-rs) และ `-i ring` ว่าง

### 13.6 audit ของ template มี job ถาวรแล้ว

`.github/workflows/ci.yml` ของ **kodegen เอง** เพิ่ม job `audit-template`:
`cargo audit` ใน `working-directory: templates/hexagonal`. เดิม lock ของ template
ถูก audit ก็ต่อเมื่อมีคน generate โปรเจกต์แล้ว push (job `audit` ของ template)
— ซึ่งช้าเกินไปสำหรับไฟล์ที่อยู่ใน repo นี้ ผลวัด 2→0 advisories จึงมีตัวรันซ้ำถาวร
