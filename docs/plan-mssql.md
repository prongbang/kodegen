# Implementation Plan — Phase 7: MSSQL ผ่าน tiberius

สถานะ: **ship แล้ว `5020775`** (เจ้าของสั่งเปิด phase และรับเงื่อนไข "ไม่มี ORM,
raw SQL, image ติด EULA" แล้ว)
ขอบเขต: `kodegen add mssql` — datastore slot ตัวที่สาม ที่ไม่มี ORM อยู่ข้างหลังเลย
โดยไม่แตะ port, ไม่แตะ `kodegen feature`, และไม่แตะ composition root ที่เป็น `fn`

> ทุกตัวเลขในเอกสารนี้ **วัดจริง** บน `templates/hexagonal` ที่ `8b871a4` (lock 467 package,
> rustc 1.93.1, cargo-audit advisory-db 1172 entries, 2026-07-29) ไม่ใช่การประมาณ
> ที่ยังไม่ได้วัดจะเขียนว่า "ยังไม่ได้ยืนยัน" ตรง ๆ

---

## 0. ข้อสรุปที่กลับหัวการประเมินเดิม (สองข้อ)

plan-persistence §6 ตี Phase 7 = **L** ด้วยเหตุผลสี่ข้อ วัดแล้ว **สองข้อไม่จริง**:

| เหตุผลเดิม | ความจริงวันนี้ |
|---|---|
| "ไม่มี ORM ให้ใช้เลย ต้องเขียน SQL เอง + migration เอง" | **ยังจริง** — แต่ราคาต่ำกว่าที่คิด: เส้นทาง SeaORM เลิกใช้ migration framework ไปแล้วตั้งแต่ 6a (`SCHEMA` + startup DDL) ⇒ "เขียน migration เอง" ไม่ใช่งานใหม่ มันคือ pattern ที่ ship ไปแล้วสองรอบ |
| "TLS stack คนละตัว" | **จริงและหนักกว่าที่เขียนไว้** — ไม่ใช่แค่ "คนละตัว" แต่เป็น **rustls คนละ major ที่ EOL แล้วและมี 3 advisory ที่ไม่มีทางแก้** ดู §2 นี่คือข้อที่ตัดสินทั้งแผน |
| "image 1.5GB ติด EULA รัน CI ฟรีไม่ได้" | **ผิด** — 1.58 GiB คือขนาดหลังคลาย ตอนดึงจริง 596 MiB, `ACCEPT_EULA=Y` + Developer edition ฟรี และ license terms เขียนตรง ๆ ว่าใช้บน "third party shared devices" ได้ ห้ามแค่ production ดู §7 |
| "ต้นทุนสูงสุด reuse ต่ำสุด" | **reuse สูงกว่าที่ประเมิน** — `Slot::Datastore`, `rewrite_bootstrap`+`Swap`, `OnceCell` handle, startup DDL + retry, env-gated test, compose+`--wait` มีครบและใช้ซ้ำได้ทั้งหมด งานจริงคือ *สองไฟล์ template* + *หนึ่ง Kind* + *สาม feature line* — โครงเดียวกับ 6a เป๊ะ |

→ ประเมินใหม่: **M ก้อนเดียว** ไม่ใช่ L และไม่ต้องหั่น (§9)

---

## 1. ข้อเท็จจริงที่ยืนยันแล้ว

### 1.1 tiberius (crates.io index + API ตรง ๆ, ไม่ใช่จากความจำ)

| ข้อ | ค่าจริง | ผลต่อแผน |
|---|---|---|
| เวอร์ชันล่าสุด | **0.12.3**, publish **2024-07-19** (0.11.8 backport วันเดียวกัน) | **สองปีไม่มี release** |
| repo | `github.com/prisma/tiberius` | Prisma ย้ายโฟกัสไปนานแล้ว — ถือว่า **maintenance-only ถึง unmaintained** ยังไม่มี advisory บนตัว crate เอง แต่ก็ไม่มีใครมาแก้ dependency ให้ (ดู §2) |
| ยังมีคนใช้จริงไหม | 575k downloads/90 วัน | ไม่ใช่ crate ร้าง — เป็น de-facto ตัวเดียวของ ecosystem |
| MSRV | **ไม่ประกาศ** (`rust-version` = null), edition 2021 | `resolver = "3"` จึงไม่กันมัน และไม่ต้องกัน: โค้ดปี 2024 **compile ผ่านบน rustc 1.93.1 ใน 22 วินาที** (วัดจริง ทั้ง `tiberius` และ `bb8-tiberius`) 1.88 อยู่หลังจากนั้น จึงไม่มีเหตุให้พัง — แต่ยังไม่ได้ยืนยันด้วย toolchain 1.88 ตัวจริง |
| default features | `tds73`, `winauth`, **`native-tls`** | ต้อง `default-features = false` — `winauth` ไม่มีความหมายนอก Windows |
| TLS ที่มีให้เลือก | `native-tls` (→ `async-native-tls` → openssl) · `rustls` (→ **`tokio-rustls ^0.24`**) · `vendored-openssl` (→ `opentls`) · ไม่ใส่เลย | **นี่คือทั้งหมดของ §2** |
| pool | **ไม่มีในตัว** — `Client` เป็น connection เดียว และ query ต้องการ `&mut self` | ต้องมี pool ข้างนอก |
| connection string | `Config::from_ado_string()` และ `from_jdbc_string()` — **ไม่รับ URL** | §6 |
| parameter | `@P1, @P2, ...` ผ่าน `execute(sql, &[&dyn ToSql])` — bind จริง ไม่ใช่ string interpolation | §5.3 |

### 1.2 pool crate

| crate | ล่าสุด | downloads/90d | หมายเหตุ |
|---|---|---|---|
| **`bb8-tiberius`** 0.16.0 | 2025-01-08 | 75k | บาง, `default-features = false` แล้วเหลือแค่ `with-tokio`; feature `tls` = `tiberius/rustls` (เราไม่ใช้) |
| `deadpool-tiberius` 0.1.9 | 2025-04-11 | 5.8k | API สวยกว่า แต่ 0.1.x, คนเดียว, ใช้น้อยกว่า 13 เท่า |

**เลือก `bb8-tiberius`** — ตัวที่คนใช้มากที่สุดและใหม่พอ ๆ กัน `bb8::Pool` สร้างแบบไม่ต่อสายได้
(`build_unchecked`) ซึ่งเข้ากับ composition root ที่เป็น `fn` อยู่แล้ว
ทางเลือกที่ทิ้ง: `Arc<Mutex<Client>>` ไม่มี pool เลย — ประหยัด 2 crate แลกกับการ serialize
ทุก query ของทั้ง process และต้องเขียน reconnect เอง ไม่คุ้มสำหรับ template ที่เป็น service

---

## 2. Decision: TLS — ข้อที่ตัดสินทั้งแผน

**เลือก: `native-tls` + `dep:openssl-sys` (vendored) ไม่ใช่ `rustls`**

วัดสามทางบน lock จริงของ template (regenerate ทั้งไฟล์ ตัวเลข total จึงมี drift
จาก `cargo update` ปนอยู่ 2 package ที่ไม่เกี่ยวกับ tiberius):

| ทาง | lock | crate ชื่อใหม่ | `cargo audit` |
|---|---|---|---|
| `rustls` | 487 | 13 + **rustls/rustls-webpki/tokio-rustls/rustls-native-certs เวอร์ชันที่สองในกราฟ** | ❌ **3 vulnerabilities + 1 unmaintained** |
| `native-tls` | 484 | 17 (`async-native-tls`, `native-tls`, `openssl`, `openssl-macros`, `foreign-types{,-shared}`) | ✅ **0** |
| ไม่ใส่ TLS เลย | 478 | 11 | ✅ **0** |

### 2.1 ทำไม `rustls` ตกรอบ — และทำไมมันไม่เหมือนเคส `rsa`

`tiberius/rustls` → `tokio-rustls ^0.24` → **rustls 0.21** ซึ่ง:

- เป็น **major คนละตัว** กับ rustls 0.23 ที่ทั้ง template อยู่ (mongodb/sqlx/aws-sdk-s3)
  ⇒ ไม่ใช่ปัญหา provider unification แบบ §3.1 ของ plan-seaorm — มันแย่กว่านั้น
  เพราะ rustls 0.21 **ไม่มีระบบ provider เลย** มันผูก `ring` ตายตัว ⇒ `ring` ที่ 6a
  อุตส่าห์ไล่ออกจาก build ไปแล้ว (§12.7) **กลับเข้ามาทันที** และ binary แบก TLS stack สองชุด
- `cargo audit` บน lock นั้น: `rustls-webpki 0.101.7` โดน **RUSTSEC-2026-0104**
  (reachable panic ใน CRL parsing), **RUSTSEC-2026-0098**, **RUSTSEC-2026-0099**
  (name constraints ถูกยอมรับผิด) + `rustls-pemfile 1.0.4` unmaintained (RUSTSEC-2025-0134)
- **ไม่มีทางแก้**: fix อยู่ที่ webpki 0.103.12+ ซึ่งอยู่คนละ major กับ tokio-rustls 0.24
  ที่ tiberius pin ไว้ และ tiberius ไม่มี release มาสองปี ⇒ ทางเดียวคือรอ upstream ที่ไม่ขยับ
- **เทียบกับเคส `rsa`/RUSTSEC-2023-0071 ที่เรา ignore ไปแล้วใน 6b: ต่างกันคนละเรื่อง**
  ตอนนั้นเหตุผลที่ ignore ได้คือ *อ่าน source แล้วพิสูจน์ได้ว่า code path ที่ช่องโหว่อยู่
  ไม่ถูกเรียก* (client ไม่มี private key จึงไม่มี decryption ให้จับเวลา) ส่วนสามตัวนี้อยู่ใน
  **certificate validation** ซึ่งคือเส้นทางที่เราจะใช้จริงทุกครั้งที่ต่อ TLS
  ⇒ **ignore ไม่ได้อย่างซื่อสัตย์** และการใส่ `--ignore` สามบรรทัดลง `.cargo/audit.toml`
  ที่ template ship ให้ **ทุกโปรเจกต์ที่ generate** จะเป็นการปิดตาแทนที่จะแก้

### 2.2 ราคาของ `native-tls` ที่ต้องยอมรับ

- `native-tls` → `openssl-sys` ซึ่ง default = link libssl ของระบบ **แต่ runtime image เป็น
  distroless/cc ที่ไม่มี libssl** ⇒ feature `mssql` **ต้อง** เปิด `dep:openssl-sys` ด้วย
  (template ประกาศมันไว้แล้วพร้อม `vendored` สำหรับ diesel pg/mysql)
  **ยืนยันแล้วด้วย `cargo tree -e features -i openssl-sys`**: feature unification ทำงานจริง —
  `openssl-sys` ได้ทั้ง `default` และ `vendored` และ `vendored`→`openssl-src` ชนะ
  ทั้งกรณี `--features mssql` เดี่ยว ๆ และกรณี `--features mssql,postgres` พร้อมกัน
- ⇒ build stage ต้องมี perl/make **แบบเดียวกับที่ `add postgres` แทรกอยู่แล้ว** (anchor `protobuf-compiler`)
  ใช้ code path เดิม ไม่มี const ใหม่ — นี่คือการเสียจุดขาย "pure Rust ไม่ต้องมี C toolchain"
  ของเส้นทาง SeaORM ไป และต้องเขียนใน `--list` ให้ผู้ใช้เห็นก่อนเลือก
- ⚠️ **กับดักที่ CI จะจับไม่ได้ (คลาสเดียวกับ §3.1 ของ plan-seaorm)**: vendored OpenSSL
  compile มาพร้อม `OPENSSLDIR` ที่ชี้ไปยัง build dir ซึ่งไม่มี CA cert ⇒ ต่อ **Azure SQL
  หรือ server ที่ใช้ cert จริง** จะ verify ไม่ผ่าน ขณะที่ dev/CI ใช้
  `TrustServerCertificate=true` จึงเขียวตลอด ทางแก้: `ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt`
  ใน runtime stage ของ Dockerfile (OpenSSL อ่าน env ตัวนี้เหนือ compiled-in path)
  **ยังไม่ได้ยืนยันว่า distroless/cc มีไฟล์นี้จริง — ต้องตรวจก่อน implement** ถ้าไม่มี
  ทางแก้คือ copy จาก build stage
- `winauth` ปิด, `sql-browser` ปิด, `chrono`/`time`/`rust_decimal`/`bigdecimal` ปิดหมด
  (ตาราง `users` เป็น NVARCHAR ล้วน แบบเดียวกับที่ 6a ปิด default ของ sea-orm)

### 2.3 ทางที่ไม่เลือก

- ❌ **ไม่ใส่ TLS เลย** (lock เล็กสุด, audit สะอาด, `encrypt=DANGER_PLAINTEXT`) —
  แปลว่า login packet และรหัสผ่านวิ่ง plaintext (TDS obfuscate password ด้วยการสลับ nibble
  ไม่ใช่การเข้ารหัส) การ ship adapter ที่ **คุยแบบเข้ารหัสไม่ได้เลย** คือการลดความปลอดภัย
  ไม่ใช่การลดขนาด — ตัดทิ้ง
- ❌ **`vendored-openssl` feature ของ tiberius** — มันคือ `opentls` ที่ผูก `io-async-std`
  ไม่ใช่ทางเดียวกับ `native-tls` + `openssl-sys{vendored}` ที่ template ทำอยู่แล้ว

---

## 3. Decision: ตำแหน่งใน model

### เลือก: `Kind::Mssql` ใหม่, slot `Datastore`, `takes_orm() == false`

```rust
enum Capability { ..., Mssql }
enum Kind { Relational(Relational), Mssql, Document, Cache, Blob, Events(EventBackend) }
```

**ทำไมไม่ใช่ `Relational::Mssql`** — `Relational` ถูกแยกออกมาเพื่อให้ทุกอย่างที่ต่างตาม dialect
*total*: `feature(orm)`, `startup_ddl()`, `up_sql()`, `conflict_target()`, apt package ของ C driver,
`DieselConnection` ตัวไหน MSSQL **ไม่มีคำตอบให้สักข้อ** — ไม่มี Diesel backend, ไม่มี SeaORM backend,
ไม่มี migration dir, ไม่มี `ON CONFLICT` ใด ๆ ใส่เข้าไปคือบังคับให้ `Relational` เริ่มโกหก
ซึ่งเป็นเหตุผลเดียวกันเป๊ะกับที่ doc comment ของ `Kind` เขียนไว้ว่าทำไม MongoDB ไม่ใช่ `Relational` ตัวที่สี่

**`Kind::Mssql` ไม่ใช่ `Kind::Tds`** — ตั้งชื่อตาม vendor เพราะมันมี vendor เดียวจริง ๆ
(`cache`/`timeseries`/`blob` ตั้งชื่อตามรูปทรงเพราะครอบหลายเจ้า) Azure SQL Database และ
Azure SQL MI คือปลายทางเดียวกันที่ tiberius ต่อได้ฟรี — ความสัมพันธ์แบบเดียวกับ Valkey ⊂ redis
วันที่มี backend ที่พูด TDS แต่ไม่ใช่ SQL Server ค่อย rename

### สิ่งที่ totality ที่มีอยู่บังคับให้ครบ *ฟรี*

| กลไกเดิม | สิ่งที่ต้องเติม | ได้อะไร |
|---|---|---|
| `Kind::slot()` | `Kind::Mssql => Some(Slot::Datastore)` | `add mssql` บนโปรเจกต์ postgres/mongo/seaorm → refuse; และกลับกันทุกทิศ **โดยไม่เขียนกฎใหม่** |
| `Slot::Datastore::features()` | เติม `"mssql"` | ข้อบน + test ที่ pin `Slot::features()` ↔ `Capability::feature()` ครอบให้ |
| `Capability::feature(orm)` | `Kind::Mssql => "mssql"` (ไม่สน orm) | — |
| `Capability::takes_orm()` | `false` อัตโนมัติ (มันคือ `matches!(kind(), Kind::Relational(_))`) | `--orm` → error ก่อนอ่านไฟล์ใด ๆ |
| `add()` match บน `(kind, orm)` | arm `(Kind::Mssql, _)` | compile error ถ้าลืม |
| `datastore_conflict` | arm ที่ `other == "mssql"` | ใช้ closure `blocks(...)` ตัวเดิม — MSSQL เข้ากลุ่ม "สลับ block ไม่ใช่สลับไฟล์" เหมือน document/seaorm |

### จุดเดียวที่ต้องเขียนข้อความใหม่: `check_orm`

ข้อความปัจจุบันคือ *"--orm applies only to relational capabilities (postgres, mysql, mariadb,
timescaledb) — 'redis' has no ORM"* ซึ่งถ้าเอามาใช้กับ `mssql` จะ **ผิดความจริงและชวนสับสน**:
MSSQL *เป็น* relational แต่ไม่มี ORM ⇒ ต้องแยกเป็น arm ของตัวเอง

```
--orm does not apply to 'mssql': neither Diesel nor SeaORM supports SQL Server in
open source (diesel#2201 has been open since 2019; SeaORM's MSSQL backend is in
the commercial SeaORM X), so `kodegen add mssql` writes raw SQL against tiberius.
That is the trade — see `kodegen add --list`.
```

`check_orm` จึงกลายเป็น match สาม arm (relational / mssql / ที่เหลือ) ไม่ใช่ `if` สองชั้น —
รูปเดียวกับที่ §13.3 ของ plan-seaorm บอกว่าอยากให้มันเป็น

---

## 4. Decision: handle + pool

### เลือก: `Db` (ชื่อเดิมตามกติกา §7 ของ plan-seaorm) = `Arc<Inner>` + `OnceCell<Pool<...>>`

```rust
pub struct Db(Arc<Inner>);
struct Inner { ado: String, pool: OnceCell<bb8::Pool<bb8_tiberius::ConnectionManager>> }
impl Db {
    pub fn new(ado: impl Into<String>) -> Self { ... }          // sync, ไม่ต่อสาย
    async fn pool(&self) -> Result<&Pool<...>, DomainError> {
        self.0.pool.get_or_try_init(|| async {
            let mgr = ConnectionManager::build(config(&self.0.ado)?)?;
            let pool = Pool::builder().max_size(8).build(mgr).await?;
            { let mut c = pool.get().await?;
              for stmt in SCHEMA { /* retry-once ตาม §5.2 */ } }
            Ok(pool)
        }).await
    }
}
```

- ตั้งชื่อ `Db` ⇒ `scaffold.rs` **ไม่ต้องแก้แม้แต่บรรทัดเดียว** และ builder ที่
  `kodegen feature` เคย scaffold ไว้ (`_db: Option<&Db>`) ยัง compile ผ่านทั้งไปและกลับ —
  กติกาที่ plan-seaorm §7 เขียนไว้ว่าให้ทำกับ handle ตัวถัดไป **นี่คือตัวถัดไปนั้น**
- ไม่ทำ `build_services_from_env()` เป็น async ด้วยเหตุผลชุดเดียวกับ §4 ของ plan-seaorm
  (จะทำให้ `bootstrap.rs` ทุกเวอร์ชันที่ generate ไปแล้วถูก refuse) — และที่นี่ยิ่งชัด
  เพราะ `Pool::builder().build()` ต่อสายจริง
- error mapping (สมมาตรกับ `mongo_repo::infra` / `sea_db::infra`):
  `tiberius::error::Error::{Io, Tls, Protocol}` และ `bb8::RunError::TimedOut` → `Unavailable` (503)
  · `Error::Server(_)` และที่เหลือ → `Other` (500) — "server ตอบมาเอง" ไม่ใช่ "ลองใหม่ทีหลัง"

---

## 5. Decision: SQL ดิบ

### 5.1 DDL — `IF OBJECT_ID(...) IS NULL` ไม่ใช่ `CREATE TABLE IF NOT EXISTS`

**SQL Server ไม่มี `CREATE TABLE IF NOT EXISTS`** (ยังไม่มีใน 2022 และไม่มีใน 2025) —
ถ้าลอกจาก `SEA_DB_TPL` มาตรง ๆ จะพังตอน runtime ไม่ใช่ตอน compile

```sql
IF OBJECT_ID(N'dbo.users', N'U') IS NULL
    CREATE TABLE dbo.users (
        id   NVARCHAR(64)  NOT NULL PRIMARY KEY,
        name NVARCHAR(255) NOT NULL
    );
```

- **`NVARCHAR` ไม่ใช่ `VARCHAR`** — tiberius ส่ง Rust `&str` เป็น NVARCHAR เสมอ ถ้าคอลัมน์เป็น
  VARCHAR ทุก `WHERE id = @P1` จะเกิด implicit conversion และ **ทิ้ง index seek กลายเป็น scan**
  (เป็น runtime/perf failure ล้วน ไม่มีอะไรเตือน) — บทเรียนคลาสเดียวกับ `TEXT PRIMARY KEY` ของ MySQL
- `NVARCHAR(64)` = 128 byte ต่ำกว่าเพดาน index key 900 byte สบาย ๆ
- ใช้ schema `dbo` ตรง ๆ ⇒ ไม่ต้อง `CREATE SCHEMA` ซึ่งเป็น statement ที่ **ต้องอยู่ต้น batch เท่านั้น**
  ส่วน `CREATE TABLE` ซ้อนใน `IF` ได้ปกติ
- ⚠️ **collation default ของ image เป็น case-insensitive** (`SQL_Latin1_General_CP1_CI_AS`)
  ⇒ id `"u1"` กับ `"U1"` ชนกันที่ PK — พฤติกรรมเดียวกับ MySQL default ต่างจาก PostgreSQL
  เขียนไว้ในหัวไฟล์

### 5.2 DDL race — ใช้ retry-once ตัวเดิม

หลาย pod boot พร้อมกันบน DB ว่าง → SQL Server ตอบ **error 2714** ("There is already an object
named 'users'") แทนที่จะเป็น no-op — คือเคสเดียวกับ `23505` บน `pg_type` ที่ §12.2 ของ
plan-seaorm เจอและแก้ไปแล้ว ⇒ **ใช้ logic เดิมทั้งดุ้น**: statement ไหน error ให้ลองซ้ำ
**หนึ่งครั้ง**, และ error ที่เป็น connection/timeout ให้ return ทันที ไม่ retry

### 5.3 Upsert — `MERGE ... WITH (HOLDLOCK)` (เลือกทางนี้)

```sql
MERGE dbo.users WITH (HOLDLOCK) AS t
USING (VALUES (@P1, @P2)) AS s (id, name)
   ON t.id = s.id
 WHEN MATCHED THEN UPDATE SET name = s.name
 WHEN NOT MATCHED THEN INSERT (id, name) VALUES (s.id, s.name);
```

**ชั่งกับทางเลือก `UPDATE ... IF @@ROWCOUNT = 0 INSERT`:**

ทางนั้น *ต้องมี transaction ชัดเจน* ถึงจะถูก — ใน autocommit ทุก statement เป็น transaction
ของตัวเอง ⇒ range lock ที่ `UPDATE WITH (UPDLOCK, SERIALIZABLE)` จับไว้ **ถูกปล่อยก่อน INSERT
จะทำงาน** และ race กลับมาเต็ม ๆ ⇒ ต้องพก `BEGIN TRAN`/`COMMIT` + `SET XACT_ABORT ON`
กันไม่ให้เหลือ transaction ค้างเมื่อ batch พังกลางคัน = สามอย่างที่ลืมได้และเงียบ

`MERGE` + `HOLDLOCK` เป็น **statement เดียว** จึง atomic โดยตัวมันเองใน autocommit —
ไม่ต้องจัดการ transaction เลย และ round trip เดียว

รู้ตัวว่ากำลังเลือกของที่ชุมชน MSSQL เตือน ("Don't use MERGE") — bug list ของมันเกือบทั้งหมด
อยู่ที่ trigger, foreign key, filtered index, `OUTPUT $action`, target ที่เป็น view
ซึ่ง **ไม่มีสักอย่างในตารางสองคอลัมน์นี้** ส่วนข้อที่เกี่ยวจริงมีสองข้อและกันได้:
`HOLDLOCK` (ขาดแล้ว racy เงียบ ๆ) และ **`MERGE` ต้องปิดท้ายด้วย `;`** (ไม่ใส่ = syntax error)
— เขียนไว้เป็น comment ข้างค่าคงที่ทั้งคู่

**สัญญาของ port ปลอดภัยกว่าเส้นทาง MySQL/SeaORM ด้วยซ้ำ**: `save` ค่าเดิมซ้ำ → `WHEN MATCHED`
→ UPDATE ทำงานจริง และเราไม่อ่าน `rows_affected` เลย ⇒ ไม่มีเคส "0 affected row กลายเป็น error"
แบบ `DbErr::RecordNotInserted` ที่ 6b ต้องไล่ปิด

**Injection**: ทุกค่าไปทาง `execute(sql, &[&id, &name])` = bind parameter จริงของ TDS
ไม่มีจุดไหนในทั้ง adapter ที่ต่อ string ⇒ ห้ามมี `format!` ในไฟล์ repo และ **มี unit test
ฝั่ง kodegen ที่ assert ว่า template ไม่มี `format!` ใน SQL** (test ราคาถูก กันของแพง)

### 5.4 ไม่มี migration framework

ตาม precedent ที่มีอยู่ **ห้าที่แล้ว** (mongo, clickhouse, s3, seaorm, และ plan-persistence §3.4
ที่เขียนล่วงหน้าไว้เองว่า MSSQL จะเป็นแบบนี้): `SCHEMA: &[&str]` + startup DDL
`migrations/` ที่ค้างจาก `kodegen new` → **warn ไม่ลบ** ใช้ข้อความรูปเดียวกับ `add_seaorm`

---

## 6. Decision: env + connection string

### เลือก: `DATABASE_URL` ตัวเดิม, ค่าเป็น **ADO.NET connection string**

```
DATABASE_URL=server=tcp:127.0.0.1,1433;user id=sa;password=kodeGen!2026;database=app;encrypt=true;TrustServerCertificate=true
```

- **ชื่อเดิม** เพราะมันคือสวิตช์ของ datastore slot ตัวเดียวกัน — precedent ตรงคือ 6a ที่ใช้
  `DATABASE_URL` ต่อจาก Diesel "เหมือนกันทุกตัวอักษร"; compose / README / CI / `Capability::env()`
  ไม่มีอะไรต้องเปลี่ยนรูป และ `Swap` เขียนทับ block ที่อ่าน var นี้อยู่แล้ว
- **ADO.NET ไม่ใช่ JDBC** เพราะเป็นรูปที่ Azure portal, SSMS และเอกสาร Microsoft ทุกฉบับพิมพ์ออกมา
  (`from_jdbc_string` มีจริงและใช้ได้ แต่มีรูปเดียวก็พอ — ผู้ใช้ที่มี JDBC string อยู่แล้วแปลงเองได้
  และ `add` พิมพ์ค่าตัวอย่างให้ตอนจบอยู่แล้ว)
- ข้อแลกที่รับ: ใครเปลี่ยนจาก postgres มา mssql แล้วลืม `postgres://...` ค้างใน env จะได้
  parse error ตอน request แรก — **ดังและอ่านออก** ไม่ใช่เงียบ (พฤติกรรม "ไม่ตั้ง = in-memory"
  ยังเหมือนเดิมทุกประการ)
- `TrustServerCertificate=true` **เฉพาะ dev/compose** เพราะ image ใช้ self-signed cert
  README ต้องเขียนตรง ๆ ว่าบน production ให้เอาออกและใช้ CA จริง (คู่กับกับดัก `SSL_CERT_FILE` ใน §2.2)
- SA password ต้องผ่าน policy ของ image: ≥8 ตัว และครบ 3 ใน 4 หมู่ (พิมพ์ใหญ่/เล็ก/เลข/สัญลักษณ์)
  ⇒ ใช้ `app:app` แบบ pg/mysql ไม่ได้ ต้องมีค่าคงที่ของตัวเอง

---

## 7. Decision: Docker / compose / CI — ข้อเท็จจริงปี 2026

**plan เดิมเขียนว่า "image 1.5GB ติด EULA รัน CI ฟรีไม่ได้" — ตรวจแล้วไม่จริง**

| ข้อ | ความจริง (ตรวจจาก MCR registry API + layer blob จริง + license PDF) |
|---|---|
| tag | `2017/2019/2022/2025-latest` มีครบ, `latest` = `2025-latest` (digest เดียวกัน) SQL Server 2025 GA 2025-11-18 |
| ขนาด | `2022-latest` = **596 MiB ตอนดึง** / 1.58 GiB หลังคลาย · `2025-latest` = 603 MiB / 1.64 GiB — "1.5GB" คือเลขหลังคลาย ไม่ใช่ค่าที่ CI ต้องโหลด |
| EULA | `ACCEPT_EULA=Y` + `MSSQL_SA_PASSWORD` (ตัวใหม่; `SA_PASSWORD` deprecated แต่ยังใช้ได้) · `MSSQL_PID` **ไม่ต้องใส่** — image baked `developer` มาแล้ว |
| license | Developer edition ฟรี และ terms เขียนว่า *"install and use any number of copies … on any device, **including third party shared devices**, to design, develop, test and demonstrate your programs"* ห้ามแค่ production ⇒ **GH Actions runner อยู่ในสิทธิ์นั้นตรง ๆ ไม่มีข้อห้ามเรื่อง CI / public repo / open source** |
| RAM | ขั้นต่ำ 2 GB · runner public 4 vCPU/16 GB, private 2 vCPU/8 GB ⇒ ผ่านทั้งคู่ (disk 14 GB คือข้อที่คับที่สุด) |
| sqlcmd | **ยังอยู่ในทั้ง 2022 และ 2025** แต่ย้ายไป `/opt/mssql-tools18/bin/sqlcmd` และ `/opt/mssql-tools/` **ถูกถอดออกแล้ว** ⇒ ตัวอย่าง healthcheck ที่ลอยอยู่บนเน็ตส่วนใหญ่พังหมด และ **ต้องมี `-C`** (tools18 encrypt-by-default เจอ self-signed cert) |
| HEALTHCHECK | image **ไม่ประกาศมาให้** ต้องเขียนเอง |
| arm64 | **ไม่มี native arm64** (manifest เป็น amd64 เดี่ยว) Microsoft ระบุว่าไม่ support emulation ด้วย ⇒ dev บน Apple Silicon ต้อง emulate |
| boot time | **ยังไม่ได้ยืนยัน** — ไม่มีตัวเลขทางการ ประมาณ 30–60 วิ ใช้วางแผนได้ แต่ห้ามอ้างเป็นข้อเท็จจริง |
| azure-sql-edge | **retired 2025-09-30** + archived docs ระบุ "no longer supports the ARM64 platform" ⇒ **ตัดทิ้ง ไม่ใช่ทางเลือกอีกต่อไป** |

### 7.1 เลือก `2022-latest` ไม่ใช่ `2025-latest`

2025 มี bug ที่ RTM–CU2: crash ตอน start เมื่อ `sockets × cores_per_socket != total_processors`
(microsoft/mssql-docker#954) และเคย crash บน Docker Desktop/Rosetta เพราะ AVX (แก้ใน CU1 แล้ว)
runner 2/4 vCPU ไม่โดน แต่เครื่อง dev หลากหลายกว่านั้น — 2022 เล็กกว่า เก่ากว่า และไม่มีข้อนี้
เขียน comment ใน compose ว่า `2025-latest` ใช้ได้และเปลี่ยน tag บรรทัดเดียว

### 7.2 compose — และปัญหาที่ pg/mysql ไม่มี: **image ไม่สร้าง database ให้**

`POSTGRES_DB` / `MYSQL_DATABASE` ไม่มีของเทียบเท่าใน image นี้ — start มาแล้วมีแค่
master/tempdb/model/msdb ⇒ `database=app` ใน connection string จะต่อไม่ติดตั้งแต่แรก

**เลือก: service ที่สองใน compose ที่รัน `CREATE DATABASE` ครั้งเดียว** (ไม่ใช่ให้แอปทำ)

```yaml
  mssql:
    image: mcr.microsoft.com/mssql/server:2022-latest
    platform: linux/amd64          # ไม่มี arm64 image — Apple Silicon จะ emulate
    environment: { ACCEPT_EULA: "Y", MSSQL_SA_PASSWORD: "kodeGen!2026" }
    ports: ["1433:1433"]
    healthcheck:
      test: ["CMD-SHELL", "/opt/mssql-tools18/bin/sqlcmd -S localhost -C -U sa -P \"$$MSSQL_SA_PASSWORD\" -Q 'SELECT 1' -b -o /dev/null"]
  mssql-init:                       # image เดิม ไม่มีอะไรต้องดึงเพิ่ม
    depends_on: { mssql: { condition: service_healthy } }
    restart: "no"
    entrypoint: [... "-Q", "IF DB_ID('app') IS NULL CREATE DATABASE app"]
```

เหตุผลที่ไม่ให้แอปสร้าง database เอง: ต้องต่อเข้า `master` ก่อนแล้วต่อใหม่เข้า `app` = config
สองชุดในโค้ดที่มี handle เดียว และในโปรดักชันจริง role ของแอปแทบไม่เคยมีสิทธิ์ `CREATE DATABASE`
อยู่แล้ว — มันเป็น deployment step ไม่ใช่งานของแอป (เขียนไว้ใน README ให้ตรงกัน)

⚠️ **ต้องตรวจก่อน implement**: `docker compose up -d --wait` ปฏิบัติกับ one-shot service ที่
exit 0 อย่างไร (trick ที่แถว influxdb/seaorm ใช้อยู่) ถ้า `--wait` ไม่ยอม ให้ CI สั่ง
`up -d --wait mssql` แล้วตามด้วย `run --rm mssql-init` — สองบรรทัด ไม่กระทบดีไซน์อื่น

### 7.3 CI — หนึ่งแถว matrix ตามธรรมเนียม

แถว `mssql` ทำแบบเดียวกับแถว seaorm: boot จาก compose ที่ `add` เพิ่งเขียน, export
`DATABASE_URL` เข้า `$GITHUB_ENV` เฉพาะแถวนั้น, แล้ว **รัน `cargo test` สองรอบทับ DB เดิม**
(รอบสองพิสูจน์ทั้ง DDL idempotent และ upsert ค่าเดิมซ้ำ — สองบทเรียนของ 6a/6b ในหนึ่ง step)
job `features` ของ template เพิ่มแถว `mssql` ได้ปกติ (ไม่มี `compile_error!` มาชนเหมือน seaorm)

---

## 8. Decision: อะไรถูก generate / rewrite

| ไฟล์ | การเปลี่ยนแปลง |
|---|---|
| `Cargo.toml` (template) | optional deps 3 ตัว + feature `mssql` |
| `src/infrastructure/mssql_db.rs` | **สร้างใหม่** — `Db` + `OnceCell<Pool>` + `SCHEMA` + retry + `infra()` |
| `src/infrastructure/mssql_repo.rs` | **สร้างใหม่** — `MssqlUserRepository` (MERGE + SELECT) + env-gated tests |
| `src/infrastructure/mod.rs` | append `#[cfg(feature = "mssql")] pub mod ...` ×2 |
| `src/bootstrap.rs` | สลับสาม block ผ่าน `rewrite_bootstrap(&MSSQL_SWAP)` (verbatim หรือ refuse) |
| `Dockerfile` | **แตะ** — perl/make ที่ anchor เดิม (code path เดียวกับ `add postgres`) + `ENV SSL_CERT_FILE` |
| `docker-compose.dev.yml` | append 2 service (§7.2) |
| `migrations/` | ไม่แตะ + warn |
| `src/lib.rs` | **ไม่แตะ** — `compile_error!` ปัจจุบันคุม relational×seaorm; MSSQL ไม่ชนกับใครเพราะ `mssql_db.rs` ถูก cfg ปิดทั้งไฟล์ (ถ้าอยากได้ guard ที่สามค่อยคุยแยก ดู §11) |

```toml
mssql = ["dep:tiberius", "dep:bb8", "dep:bb8-tiberius", "dep:openssl-sys", "tokio/sync"]
```

`MSSQL_SWAP`: `builder_handle: None` (handle ชื่อ `Db` เหมือนเดิม), `leftovers: &["diesel", "Diesel"]`
— **ก๊อป guard ของ SeaORM มาทั้งดุ้นด้วยเหตุผลเดียวกัน**: type ชื่อเดิมแต่ API คนละตัว
ใครเรียก `db.run(...)` เองจะพัง ⇒ ต้องมี positive-path test บน `bootstrap.rs` ของ template สด
แบบ `the_seaorm_guard_passes_on_a_fresh_bootstrap_and_refuses_a_spiked_one`
(และ `BOOTSTRAP_*_MSSQL` ทั้งสามก้อน **ห้ามมีคำว่า Diesel ใน comment**)

---

## 9. Phasing

### เลือก: **ก้อนเดียว, M** — ไม่หั่น

เกณฑ์ของ plan-persistence คือ "แต่ละก้อน ship/test/docker-gate ได้เอง" ก้อนเดียวนี้ผ่านครบ
และ **ไม่มีอะไรที่ก้อนที่สองจะพิสูจน์แทนก้อนแรกไม่ได้**: 6 ถูกหั่นเป็น 6a/6b เพราะ
`ON DUPLICATE KEY UPDATE` ของ MySQL เป็น runtime behavior ที่ PostgreSQL พิสูจน์แทนไม่ได้
— MSSQL มี backend เดียว dialect เดียว ⇒ ไม่มีเส้นแบ่งแบบนั้นให้ใช้

ประเมินบรรทัด: `capability.rs` +450–550 (สองในสามเป็น template constant), `main.rs` **+0**
(`Capability` เป็น `clap::ValueEnum` — เพิ่ม variant พอ), template `Cargo.toml` +12,
compose const +25, CI +14, README ×2 +40

**เกณฑ์จบ**: `kodegen add mssql` → generated project ผ่าน fmt / clippy `-D warnings` /
`test --locked` · ไม่ตั้ง `DATABASE_URL` ยังได้ in-memory · ตั้งแล้วต่อ SQL Server จริง
สร้าง schema เอง upsert ได้ และ **`cargo test` รอบสองบน DB เดิมเขียว** ·
`cargo audit` = 0 unignored (ไม่มี ignore ใหม่) · lock ไม่ขยับตอนพลิก feature ·
`--orm` ทุกค่ากับ `mssql` → error ที่อ่านออก

---

## 10. Decision: Testing

หลักเดิม: capability × default + จุดที่พิสูจน์อะไรใหม่เท่านั้น

**unit test ฝั่ง kodegen** (ต่อยอด fixture เดิมทั้งหมด ไม่มีกลไกใหม่):
`--orm diesel|seaorm` บน `mssql` → error ก่อนแตะไฟล์ · `add mssql` บนโปรเจกต์
postgres/mongo/seaorm → refuse พร้อมสามขั้นตอน (และกลับกันทุกทิศ) ·
`enable_feature` เปลี่ยน `["sqlite"]` → `["mssql"]` และ **ไม่แตะ `[dependencies]`** ·
`bootstrap.rs` ที่แก้มือ → refuse ไม่สลับครึ่งเดียว · guard positive-path บน template สด ·
หลัง `add mssql` แล้ว `feature notification` ยัง scaffold ได้และได้ `Option<&Db>` ·
`--dry-run` ไม่เขียนไฟล์ · test ที่ pin `Slot::features()` ↔ `Capability::feature()`
ต้องเห็น `"mssql"` · **test ใหม่หนึ่งตัว: `MSSQL_REPO_TPL` ไม่มี `format!` (§5.3)**

**test ในโปรเจกต์ที่ generate**: gate ด้วย `DATABASE_URL` ไม่ใช้ `#[ignore]` ตามเดิม
และต้องมี `save_twice_identical_values_is_ok` แบบ 6b (test เดียว เขียนซ้ำในตัวมันเอง)

**handle ต่อ test ไม่ share ข้าม test** — บทเรียน §12.3 ของ plan-seaorm (`#[tokio::test]`
สร้าง runtime ต่อ test แล้ว drop; pool ผูกกับ runtime ที่สร้างมัน) ใช้กับ bb8 เหมือนกันทุกประการ

---

## 11. สิ่งที่จะ *ไม่* ทำ (ตั้งใจ)

- ❌ **`--orm` บน mssql ทุกกรณี** — error ไม่ใช่ ignore และมีข้อความของตัวเอง (§3)
- ❌ **migration framework** — startup DDL ตาม precedent ห้าที่ (§5.4); ประกาศเป็นข้อแลกที่
  ผู้ใช้เห็นก่อนเลือก ต้องเขียนใน `--list`, README ทั้งสองไฟล์ และหัว `mssql_db.rs`
- ❌ **`UPDATE ... IF @@ROWCOUNT = 0 INSERT`** — ต้องพก transaction + `XACT_ABORT` ถึงจะถูก (§5.3)
- ❌ **`tiberius/rustls`** — 3 advisory ที่ไม่มีทางแก้ + rustls สอง major + ring กลับเข้ามา (§2.1)
- ❌ **ต่อแบบ plaintext เป็นค่า default** (§2.3)
- ❌ **named instance / SQL Browser** (`sql-browser-tokio`) — dev ใช้ port ตรง, prod ใช้ FQDN:port
- ❌ **Windows / Integrated auth / Kerberos** (`winauth`, `integrated-auth-gssapi`) —
  runtime image เป็น Linux distroless และ `libgssapi` เป็น C dep อีกตัว
- ❌ **azure-sql-edge เป็นทางเลือก image ที่เบากว่า** — retired ไปแล้ว (§7)
- ❌ **`Relational::Mssql`** (§3) และ **abstraction ครอบ "SQL dialect"** — `UserRepository` คือ
  abstraction ที่มีอยู่แล้ว
- ❌ **`compile_error!` guard ตัวที่สาม** — MSSQL อยู่คนละ cfg กับทั้งสอง ORM และไม่มี type
  ชนกัน (คนละโมดูล) เปิดพร้อมกันด้วย `--features` เองแล้วได้ binary ที่แบก driver เกิน
  แต่ compile ผ่านและ `bootstrap.rs` import ตัวเดียว — ราคาต่ำกว่าการเพิ่มกฎที่สาม
  ถ้าวันหนึ่งอยากปิดจริง ให้ทำเป็น "หนึ่ง datastore ต่อ binary" กฎเดียวครอบทั้งสามแทน
- ❌ **stored procedure / bulk insert (`BulkLoad`) / `OUTPUT` clause** — นอกขอบเขต seed schema

---

## 12. ความเสี่ยงที่เหลือ / ต้องเฝ้า (เรียงตามลำดับ)

1. **tiberius ไม่มี release มาสองปี และเป็นตัวเลือกเดียวที่มี** — ถ้าวันหนึ่งมี advisory
   บนตัว crate เอง จะไม่มีที่ให้ไปเลย (ต่างจาก sea-orm ที่ยังมี 2.x รออยู่)
   นี่คือความเสี่ยงที่ *รับไว้โดยรู้ตัว* ตอนเปิด phase นี้ — เฝ้าผ่าน job `audit`/`audit-template`
   ที่มีอยู่แล้ว และเขียนไว้ใน README ว่า MSSQL คือ capability ที่ ecosystem บางที่สุด
2. **vendored OpenSSL ไม่มี CA store → พังเฉพาะกับ server ที่ใช้ cert จริง (Azure SQL)**
   และ **CI จับไม่ได้** เพราะ compose ใช้ `TrustServerCertificate=true` (§2.2)
   คลาสเดียวกับ rustls-provider ของ 6a เป๊ะ ⇒ ต้องตรวจ `SSL_CERT_FILE` + ยืนยันว่า
   distroless/cc มี `/etc/ssl/certs/ca-certificates.crt` **ก่อน** merge ไม่ใช่รอ CI
3. **`MERGE` ที่ลืม `HOLDLOCK` = racy เงียบ ๆ** — ผ่าน test ทุกตัวและพังเฉพาะตอนมี
   concurrent writer จริง ⇒ comment ข้างค่าคงที่ + ห้ามแก้โดยไม่อ่าน
4. **`NVARCHAR` vs `VARCHAR`** — เป็น perf failure ที่ไม่มีอะไรเตือน (§5.1)
5. **Dockerfile ถูกแตะ** — เส้นทางนี้เสียจุดขาย "ไม่ต้องมี C toolchain" ของ SeaORM กลับไป
   และ build stage ช้าลงจาก vendored OpenSSL; ถ้าเจ็บจริงคือ signal เดียวกับที่ plan-persistence
   §7 ข้อ 2 บันทึกไว้ ไม่ใช่ปัญหาใหม่
6. **`CREATE DATABASE` เป็น step นอกแอป** (§7.2) — ผู้ใช้ที่ deploy โดยไม่อ่าน README จะเจอ
   503/500 ตอน request แรกโดยไม่รู้ว่าทำไม ⇒ ข้อความ error ของ `infra()` ต้องปล่อยข้อความ
   ของ SQL Server ("Cannot open database ... requested by the login") ออกมาให้เห็น ไม่กลืน
7. **arm64 ไม่มี** — dev บน Apple Silicon จะช้าและ Microsoft ไม่ support ⇒ `platform: linux/amd64`
   ใน compose + เขียนใน README; เป็น capability เดียวในชุดที่มีข้อนี้
8. **image 596 MiB ต่อ CI run** — แถว matrix นั้นจะช้ากว่าเพื่อน; ยังไม่ได้วัดว่าเท่าไร
9. **lock 467 → ~482** และ `openssl-src` เป็นก้อน compile ใหม่ที่ใหญ่ที่สุดในแถวนั้น

---

## 13. CTO amendments (approve-with-changes) และผลตรวจจริงตอน implement

สถานะ: **implement แล้ว, ship ที่ commit `5020775`** (branch `feat/kodegen-add-capability-foundations`)
ทุกตัวเลขในหัวข้อนี้ **วัดจริง 2026-07-29** ไม่ใช่การประมาณ

### 13.1 ข้อบังคับที่ CTO เพิ่ม และสถานะ

| # | ข้อ | สถานะ |
|---|---|---|
| 1 | ตรวจ `up --wait` + one-shot init และ CA bundle ใน distroless **ก่อน** เขียนโค้ด | ✅ ทำก่อน — ผลอยู่ §13.2 ทั้งสองข้อ **กลับผลที่แผนเดาไว้** |
| 2 | CA-store assertion ถาวรใน CI | ✅ step `The runtime image must carry a CA store for the static OpenSSL` ในแถว `mssql` — **ใช้ `docker run --entrypoint` ไม่ได้** ดู §13.2(ข) |
| 3 | Unmaintained notice 3 บรรทัด (ข้อเท็จจริง / เฝ้าผ่าน audit / trigger ถอนที่ประกาศล่วงหน้า) | ✅ `kodegen add --list`, README ทั้งสองไฟล์ |
| 4 | MERGE trapdoor doc | ✅ comment เหนือ `UPSERT` ใน `mssql_repo.rs` + ย่อหน้าใน README ทั้งสองไฟล์ |
| 5 | ห้ามเพิ่ม ignore ใน `audit.toml` สาย tiberius | ✅ `cargo audit` = exit 0, `.cargo/audit.toml` **ไม่มี diff** (ยัง 1 entry เดิมของ `rsa`) |
| 6 | ไม่ทำ JDBC / tag matrix เก่า / abstraction เผื่อ driver อื่น | ✅ ไม่ได้ทำ |

### 13.2 ผลตรวจข้อ 1 — ทั้งสองข้อ**แผนเดาผิด/ไม่ครบ**

**(ก) `docker compose up -d --wait` กับ one-shot init service**

แผน §7.2 เขียนว่า "ถ้า `--wait` ไม่ยอม ให้ CI สั่งสองบรรทัด" — **มันไม่ยอมจริง**:

```
docker compose up -d --wait            (ทั้งไฟล์)  → exit 1   ทั้งที่ init exit 0 และ DB ถูกสร้างแล้ว
docker compose up -d --wait mssql                  → exit 0
docker compose run --rm mssql-init                 → exit 0
```

`--wait` ตีความ container ที่ไม่ได้ running แล้วว่าเป็น dependency ที่ล้มเหลว
⇒ **fallback ของแผนกลายเป็นทางหลัก** ทั้งใน CI, README ทั้งสองไฟล์ และ comment ในตัว compose const เอง
(ทดลองซ้ำด้วย compose สังเคราะห์ alpine ก่อน แล้วยืนยันบน compose จริงที่ `add` เขียน)

**(ข) CA bundle ใน `gcr.io/distroless/cc-debian12:nonroot`**

- **มีจริง**: `/etc/ssl/certs/ca-certificates.crt` = 216,591 bytes, **142 certificates**
  ⇒ `ENV SSL_CERT_FILE=/etc/ssl/certs/ca-certificates.crt` ชี้ไฟล์ที่มีอยู่ **ไม่ต้อง copy จาก build stage**
  (แผน §2.2 เผื่อทางไว้ — ไม่ต้องใช้)
- **แต่ `docker run --entrypoint` ตามที่ CTO สั่งไว้ ทำไม่ได้**: distroless ไม่มี shell และไม่มี coreutils
  `docker run --entrypoint /bin/sh <img>` ตอบ `stat /bin/sh: no such file or directory`
  ⇒ assertion ที่ image ตอบได้จริงคือ **`docker create` + `docker cp`** (อ่านไฟล์ออกมาโดยไม่ต้องรัน)
  CI step จึงเช็คสามอย่าง: `SSL_CERT_FILE` อยู่ใน image config, ไฟล์ไม่ว่าง, และมี > 100 certificate
  ยืนยันแล้วบน image ที่ `docker build` จากโปรเจกต์ที่ generate จริง

### 13.3 จุดที่แผนขัดกับความจริง (นอกเหนือจาก §13.2)

1. **`Pool::builder().build()` ไม่ได้ต่อสาย** — แผน §4 สเก็ตช์ `build(mgr).await?` โดยเข้าใจว่ามันจะคืน error จริง
   ความจริง: bb8 `min_idle` default = `None` → `unwrap_or(0)` ⇒ `build()` เปิด **ศูนย์** connection
   และเท่ากับ `build_unchecked()` ทุกประการ ⇒ error แรกที่ caller เห็นคือ `RunError::TimedOut` หลัง **30 วินาที**
   โดยข้อความของ SQL Server ถูกทิ้ง — ซึ่งพังข้อ §12.6 ของแผนเองพอดี (ข้อความ "Cannot open database ..." ต้องไม่ถูกกลืน)
   **แก้**: `.min_idle(1).retry_connection(false).build(...)` — `retry_connection(false)` เพราะ bb8 default
   จะ retry แบบ backoff จนครบ `connection_timeout` (30s) ก่อนรายงาน ทั้งที่ชั้นบนมี `get_or_try_init`
   ที่ไม่ cache failure อยู่แล้ว ⇒ retry ซ้ำซ้อนและซื้อแต่ latency
   วัดผลก่อน/หลัง: รหัสผ่านผิด **30–41 วิ → 0 วิ**, และ status เปลี่ยนจาก 503 (ผิด) เป็น 500 (ถูก)
2. **`TiberiusError::Tls(_)` ไม่ควรเป็น `Unavailable`** — แผน §4 จัดไว้กลุ่มเดียวกับ `Io`/`Protocol`
   แต่ handshake ที่ล้มแปลว่า **ต่อถึงเซิร์ฟเวอร์แล้ว** และปัญหาคือ cert/trust ซึ่ง retry ไม่ช่วย
   ตอบ 503 คือบอกว่า "เดี๋ยวลองใหม่" กับความผิดพลาดของ deployment ตลอดกาล และกลืนข้อความที่ระบุสาเหตุ
   ⇒ **ย้ายไป `Other` (500)** ให้ตรงหลักที่ template ประกาศไว้เองว่า "เฉพาะเซิร์ฟเวอร์ที่ต่อไม่ถึงคือเคส 503"
   (`Io`/`Protocol` ยังเป็น `Unavailable`)
3. **native-tls บน macOS ไม่ใช่ OpenSSL** — แผน §2.2 พูดถึง openssl-sys/vendored อย่างเดียว
   ความจริง `native-tls` ผูก **Apple Secure Transport** บน macOS (openssl-sys ที่อยู่ใน lock ไม่เปลี่ยนข้อนี้
   และ `native-tls/vendored` ก็ไม่ช่วย — feature นั้นเป็น no-op บน Apple)
   ผลคือบน host macOS: `TrustServerCertificate=true` **ต่อไม่ได้เลย** (`Tls("connection closed via error")`)
   เพราะ tiberius 0.12.3 สั่ง `use_sni(false)` คู่กับ `danger_accept_invalid_certs` ซึ่ง Secure Transport ไม่ยอม
   — พิสูจน์ว่าเป็นเส้นทาง TrustAll ล้วน ๆ: ถ้า **ไม่** ใส่ `TrustServerCertificate` handshake เดินได้จนถึง
   validation แล้วตอบ `"The validity period in the certificate exceeds the maximum allowed."` (นโยบายของ Apple)
   บน Linux (runtime image + CI) ทุกอย่างทำงานปกติ ⇒ **ไม่ block capability** แต่เขียนเตือนไว้สามที่:
   หัว `mssql_db.rs`, comment ในตัว compose service, และ README ทั้งสองไฟล์ พร้อมทางออก
   (รันใน container หรือใช้ `encrypt=DANGER_PLAINTEXT` เฉพาะกับ container บนเครื่องตัวเอง)
4. **lock regen ไม่ต้อง `cargo generate-lockfile`** — วิธีนั้นได้ 484 package และมี drift 2 ตัวที่ไม่เกี่ยว
   แค่ `cargo metadata` บน lock เดิมพอ: **467 → 482** เพิ่ม 15 package แบบ additive ล้วน
   (`diff` = 176 insertions, 0 deletions, ไม่มี version ไหนขยับ) — ตรงกับที่แผนประเมินไว้ (~482)
5. **`async-std` ไม่เข้ากราฟ** ทั้งที่ tiberius ขอ `async-native-tls` ด้วย feature `runtime-async-std` — ยืนยันจาก lock จริง
6. **ข้อความที่ `add` พิมพ์ท้ายคำสั่งต้อง quote** — ADO string มี `;` ซึ่ง shell อ่านเป็นตัวคั่นคำสั่ง
   ⇒ `env_line` ใน `main.rs` ใส่ single quote เฉพาะค่าที่ไม่ใช่ bare word (capability อื่น ๆ ได้บรรทัดเดิมทุกตัวอักษร มี test pin)

### 13.4 ผลรัน gate ทั้งหมด

| gate | ผล |
|---|---|
| `cargo test` ของ kodegen | **101 passed / 0 failed** (เพิ่มใหม่ 10 ตัว) |
| E2E `new m7 && feature order && add mssql` | fmt ✅ · clippy `--locked -D warnings` ✅ · `test --locked` (ไม่มี env) ✅ |
| lock ของโปรเจกต์ที่ generate | ต่างจาก template แค่บรรทัด `name = "m7"` และ **ไม่ขยับหลัง build/test** |
| Docker gate (SQL Server 2022 จริง, amd64) | `cargo test` **สองรอบทับ DB เดิม** เขียวทั้งคู่ (12 + 5 + 1 tests) |
| HTTP roundtrip (`cargo run` บน Linux) | POST 201 → GET Ada → POST ชื่อใหม่ → GET Grace → **POST ค่าเดิมซ้ำ 201** → GET Grace → GET ที่ไม่มี 404 |
| negative: dead port | **503** ใน 0 วิ |
| negative: self-signed + TrustServerCertificate ปิด | **500** ใน 0 วิ |
| negative: database ไม่มี | **500** ใน 1 วิ + log มี `Cannot open database "nope" requested by the login. code=4060` |
| negative: รหัสผ่านผิด | **500** ใน 0 วิ |
| `cargo audit` (templates/hexagonal) | **exit 0, 0 unignored, 0 ignore ใหม่** |
| CA assertion บน image ที่ build จริง | `SSL_CERT_FILE` ใน config ✅ · ไฟล์ 216,591 bytes / 142 certs ✅ |
| MSRV (`cargo metadata --all-features`) | สูงสุดในกราฟ **1.88.0** (aws-sdk-s3) = floor เดิม ไม่ขยับ |
| ลำดับ feature↔add ทั้งสองทิศ | `feature` ก่อน `add` ✅ · `add` ก่อน `feature` ✅ (compile ผ่านทั้งคู่, ได้ `_db: Option<&Db>`) |
| อยู่ร่วม redis/s3/clickhouse | ✅ `default = ["cache", "mssql", "blob", "analytics"]` |
| regression | seaorm-postgres ✅ · diesel-mysql ✅ · mongodb ✅ · redis+s3 ✅ |
| `--orm` ทุกค่า | `add mssql --orm ...` → ข้อความของตัวเอง ✅ · `new --with mssql --orm seaorm` → refuse ✅ |

### 13.5 หนี้ที่เหลือ (ไม่ได้แก้ เพราะนอกขอบเขต phase)

- `cargo fmt --check` และ `cargo clippy` บน **ตัว kodegen เอง** แดงอยู่ก่อนแล้ว (126 fmt diff และ
  `collapsible_if` ที่ `main.rs:178` วัดที่ base commit) — CI ของ repo นี้ไม่ได้ gate สองอย่างนั้น
  โค้ดใหม่เขียนตามสไตล์ที่ไฟล์ใช้อยู่ ไม่ได้ไปรัน rustfmt ทับของเดิม

### 13.6 รอบแก้หลัง QA 58/59 + reviewer (0 Critical / 3 Warnings)

| # | เรื่อง | แก้ที่ไหน | ยืนยันยังไง |
|---|---|---|---|
| 1 | **`name NVARCHAR(255)` แคบกว่า domain** — `User::new` ยอม name ถึง **256 bytes** ⇒ ชื่อ 256 ตัวเป็น User ที่ valid แต่ SQL Server รันด้วย `ANSI_WARNINGS ON` จึงตอบ error 2628 (ไม่ truncate) = 500 เฉพาะ backend นี้ | `SCHEMA` ใน `MSSQL_DB_TPL` → `NVARCHAR(256)` (256 UTF-16 unit ครอบ 256 UTF-8 byte พอดี: อักขระที่กว้างสุดกิน 4 byte / 2 unit) + comment อธิบายว่าความกว้างมาจาก domain ไม่ใช่เลขกลม | `sys.columns` ตอบ `name nvarchar(256)` · POST ชื่อ 256 byte → **201**, อ่านกลับได้ `LEN = 256` · POST 257 byte → **400 "name too long"** (domain กันก่อนถึง SQL) |
| 2 | `new --with mssql --orm seaorm` ได้ข้อความ generic ทั้งที่ผู้ใช้เพิ่งพิมพ์ชื่อ SQL database | `check_combination` เรียก `check_orm` ก่อน fallback เมื่อเจอ `Kind::Mssql` ใน list | test เดิมแก้ให้คาด `--orm does not apply to 'mssql'` + เพิ่มเคส list ที่ไม่มี SQL เลยยังได้ข้อความ generic |
| 3 + 10 | CA assertion จ่าย `docker build` 8–15 นาที เพื่อพิสูจน์สองอย่างที่ build ไม่ได้เปลี่ยนสักอย่าง | CI step ใหม่: `grep` บรรทัด `ENV SSL_CERT_FILE` ใน Dockerfile (เป็น text property — `docker build` ทำหายไม่ได้) + `awk` เอา base image จาก `FROM` บรรทัดสุดท้าย + `docker create/cp` จาก base ตรง ๆ + นับ cert > 100 · ไฟล์ scratch ไปที่ `$RUNNER_TEMP` ไม่ใช่ build context | รัน step แบบคำต่อคำบนโปรเจกต์ที่ generate จริง: **12 วินาที**, base = `gcr.io/distroless/cc-debian12:nonroot`, 142 certificates, build context สะอาด |
| 4 | **(QA, template-wide ไม่ใช่แค่ mssql)** `DomainError::Other(_)` ถูกทิ้งทั้ง arm ⇒ 500 ที่อธิบายไม่ได้เลย | `to_http_err` ใน `templates/hexagonal/src/api/mod.rs` และ `to_status` ใน `TEST_GRPC`/`API_GRPC` template ของ `scaffold.rs` → `Other(e) => { tracing::error!(error = %e, "internal error"); ... }` · body HTTP/gRPC ไม่เปลี่ยน (ไม่ leak) | เห็นจริงที่ `RUST_LOG=info`: `ERROR m8::api: internal error error=Error forming TLS connection: ... certificate verify failed ... (self-signed certificate)` ขณะ body ยังเป็น `{"error":"internal error"}` · **regression**: anchor ของ scaffold คือ `pub struct Services {` / `.with_state(services)` / `insert_pub_mod` — ไม่มีอันไหนอยู่ใกล้ `to_http_err` (ยืนยันจาก grep call site ทั้งหมด) · โปรเจกต์ที่มี feature grpc + rest + redis: fmt ✅ clippy `-D warnings` ✅ test 20 ตัวเขียว |
| 5 | comment บอกว่า permission error ไม่ถูก retry แต่โค้ด retry (มันคือ error ที่ server ตอบ) | แก้ comment ทั้ง `sea_db` และ `mssql_db` ให้ตรงพฤติกรรม: retry หนึ่งครั้ง เสีย round trip เดียวแล้วได้คำปฏิเสธเดิม ถูกกว่ากฎที่พยายามแยกสองอย่างนี้ | — |
| 6 | assertion `!contains("VARCHAR(64) NOT NULL PRIMARY KEY")` เป็น vacuous | → `!db.contains(" VARCHAR(")` + เพิ่ม pin `name NVARCHAR(256)` | ผ่าน |
| 7 | block ของ mssql ใน template `Cargo.toml` คั่นกลาง event-sink block | ย้ายไปต่อจาก `sqlx` (datastore ด้วยกัน) ก่อน `clickhouse` | `cargo metadata --locked` exit 0, lock ไม่ขยับ (diff ยัง 176/0) |
| 8 | บรรทัดที่ `add` พิมพ์ยังเป็น `up -d` บรรทัดเดียว ไม่ตรง README/CI | `Capability::compose_up()` — สองบรรทัดเฉพาะ `Kind::Mssql`, ที่เหลือบรรทัดเดิม | output จริงตรงกับ README/CI ทุกตัวอักษร |
| 9 | env test กัน quote-in-quote | เพิ่ม assert ว่าไม่มี capability ไหนมี `'` ในค่า (single quote ซ้อนไม่ได้และ escape ไม่ได้ ⇒ วันที่มีต้องเปลี่ยนกลยุทธ์ ไม่ใช่ขยาย allow-list) | ผ่าน |

รันซ้ำทั้งหมดหลังแก้: `cargo test` **101 passed / 0 failed** · YAML ทั้งสองไฟล์ parse ผ่าน (`jobs: test, generated, generated-all, audit-template` / `ci, audit`) · compose ที่ generate parse ผ่าน (2 services) · e2e `add mssql`: fmt ✅ clippy ✅ · `cargo test` บน SQL Server 2022 จริงเขียว · lock ยัง identical กับ template ทุกบรรทัดยกเว้นชื่อ package
