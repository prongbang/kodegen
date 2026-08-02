# Implementation Plan — `kodegen add scalar` (OpenAPI preview)

สถานะ: **design เท่านั้น ยังไม่ implement**
ขอบเขต: ให้โปรเจกต์ที่ generate เสิร์ฟ OpenAPI spec ของตัวเอง + หน้า preview (Scalar) ได้
โดย spec มาจากโค้ดจริง ไม่ใช่ไฟล์ static ที่โกหกทันทีที่มี route ใหม่

ทุกตัวเลขในเอกสารนี้**วัดจริง**กับ `templates/hexagonal/` ที่ HEAD `3028ab8` ไม่ได้เดา — ดู §7

---

## 0. TL;DR

| คำถาม | คำตอบ |
|---|---|
| แหล่ง spec | **ทาง A — utoipa derive จากโค้ด** (`utoipa 5.5` + `utoipa-scalar 0.3`) |
| ทำไมไม่ใช่ static | static โกหกตั้งแต่ `kodegen feature` ครั้งแรก และไม่มีทางทำให้ซื่อสัตย์ได้โดยไม่ generate ใหม่ทั้งไฟล์ |
| Kind ใหม่ | `Kind::ApiDocs` — **ไม่มี slot** (อยู่ข้าง Cache/Blob) |
| feature flag | `apidocs = ["dep:utoipa", "dep:utoipa-scalar"]` |
| route | `GET /docs` (Scalar HTML) + `GET /openapi.json` |
| ไฟล์ใหม่ | `src/api/openapi.rs` ไฟล์เดียว |
| แก้ไฟล์เดิม | **anchor-insert ล้วน ไม่มี rewrite ไม่มี verbatim check** — ต่างจากที่ตั้งโจทย์ไว้ |
| coupling กับ `kodegen feature` | `feature` **annotate เสมอ** ผ่าน `cfg_attr` → ไม่ต้อง state-aware; state-aware จุดเดียวคือ "ถ้ามี `src/api/openapi.rs` ให้แทรก 2 บรรทัด" |
| ราคา lock | **+3 packages** (484 → 487) |
| ขนาด | **M** |

---

## 1. Decision: แหล่ง openapi.json — ทาง A (utoipa derive)

### ตัดสิน: A

เหตุผลเรียงตามน้ำหนัก:

1. **ทาง B ไม่มีคำตอบเรื่อง drift ที่ซื่อสัตย์ได้เลย** — `kodegen feature order` เพิ่ม `POST /order`,
   `GET /order/{id}` ทันที ถ้า spec เป็นไฟล์ static ต้องให้ `feature` ไป patch JSON ด้วย
   (= เขียน JSON generator ขึ้นมาอีกตัว มี template ที่สองที่ต้อง sync กับ `API_TPL`) หรือไม่ก็ยอมให้มันเก่า
   ซึ่งแปลว่าเราส่งของที่ผิดให้ผู้ใช้ตั้งแต่คำสั่งที่สอง — ไม่ใช่ trade-off มันคือ bug ที่ตั้งใจ
2. **ราคาที่กลัวไว้ ไม่มีจริง** — วัดแล้ว: lock โต **+3 packages** (`utoipa`, `utoipa-gen`,
   `utoipa-scalar`) จาก 484 เป็น 487 คิดเป็น ~0.6% ของ dependency tree ที่มีอยู่
   (`utoipa-axum` เป็นตัวที่พา `paste` มาอีกตัว — เราไม่ใช้ ดู §1.1)
3. **coupling ที่กลัวไว้ ก็ไม่มีจริง** — ดู §4: `cfg_attr` ทำให้ `kodegen feature` annotate ได้เสมอ
   โดยไม่ต้องรู้ว่าโปรเจกต์เปิด `apidocs` หรือไม่ scaffold จึง **ไม่** ต้อง state-aware แบบที่ Db/Mongo เป็น
4. **`add scalar` ไม่ต้อง rewrite ไฟล์** — `#[cfg_attr(...)]` เป็นบรรทัดที่แทรกเหนือ `pub async fn ...`
   ได้ด้วย `insert_before_line` ที่มีอยู่แล้ว จึงไม่มี non-additive edit ไม่มี verbatim compare
   ไม่มี refusal path (ต่างจาก `diesel_db.rs`) — นี่คือจุดที่โจทย์ประเมินราคาไว้สูงเกินจริง

### สถานะ crate จริง (ตรวจกับ registry, ก.ค. 2026)

| crate | version | `rust-version` | หมายเหตุ |
|---|---|---|---|
| `utoipa` | 5.5.0 | **1.75** | ต่ำกว่า template ที่ 1.88 → ไม่กระทบ MSRV เลย |
| `utoipa-scalar` | 0.3.0 | **1.75** | feature `axum` ต้องการ axum `0.8` — template อยู่ที่ 0.8.4 ✅ |
| `utoipa-axum` | 0.2.0 | — | **ไม่ใช้** ดู §1.1 |

`resolver = "3"` + `rust-version = "1.88"` resolve ได้ utoipa 5.5.0 ตรง ๆ ไม่มีการถอยเวอร์ชัน

### สิ่งที่ได้ฟรีและไม่ต้อง template

- **`components(schemas(...))` ไม่ต้องเขียน** — utoipa 5 เก็บ schema จาก usage แบบ recursive
  ทดสอบแล้วได้ `CreateUserReq`, `ErrorRes`, `User` ครบโดยประกาศแค่ `paths(...)`
- **`info.title` / `info.version` มาจาก `Cargo.toml` ของโปรเจกต์เอง** — วัดได้
  `{"title":"hexagonal-rs","version":"0.1.0"}` และ `kodegen new` rename package ให้อยู่แล้ว
  → spec ตั้งชื่อตัวเองถูกต่อโปรเจกต์โดยไม่มี placeholder แม้ตัวเดียว

### 1.1 ทำไม **ไม่** ใช้ `utoipa-axum`

`utoipa-axum` ให้ `OpenApiRouter` + `routes!()` ซึ่ง**ดีกว่าตรงจุดเดียวและสำคัญ**: path string
ถูกประกาศที่เดียว (router) แทนที่จะประกาศสองที่ (`.route("/users", ...)` และ `path = "/users"`)

แต่ราคาคือสิ่งที่โจทย์สั่งให้เลี่ยงพอดี:

- `routes()` ของทุก feature ต้องเปลี่ยน return type จาก `Router<Services>` เป็น `OpenApiRouter<Services>`
  → `API_TPL` ใน `scaffold.rs` กลายเป็น **state-aware จริง ๆ** (โปรเจกต์มี scalar ไหม = คนละ return type)
  และ `cfg_attr` ช่วยไม่ได้เพราะนี่คือ type ไม่ใช่ attribute
- `api/mod.rs::router()` เปลี่ยนรูปทั้งฟังก์ชัน → non-additive rewrite ของไฟล์ที่ `feature` แก้อยู่ทุกครั้ง

**ยอมรับ trade:** path string ซ้ำสองที่ ซึ่ง kodegen generate ทั้งคู่จาก `{{feature}}` ตัวเดียวกัน
→ scaffolded code drift ไม่ได้ มีแต่คนแก้มือเองที่ drift ได้ (ความเสี่ยงข้อ §6.4)

---

## 2. Decision: ตำแหน่งใน Kind/Slot model

### เลือก: `Kind::ApiDocs` → `slot() == None`

หลักที่เขียนไว้ใน `capability.rs` คือ **slot = "port เดียว impl เดียว"** — เอามาใช้ตรง ๆ:

- `/docs` ไม่ใช่ port ไม่มี trait ไม่มี adapter ที่ composition root สร้าง มันคือ route กับ HTML string
- ถ้าจะให้เป็น slot ต้องตอบ `Slot::features()` ได้ ซึ่งวันนี้จะได้ list ยาว 1 ตัว
  (`&["apidocs"]`) = กติกา exclusion ที่ไม่มีอะไรให้ exclude และ `Slot::shipped()` = `None`
- การ `add scalar` ซ้ำถูกปฏิเสธอยู่แล้วโดย check `capability 'scalar' already added` ที่อ่านจาก
  `default = [...]` — ไม่ต้องพึ่ง slot (Cache/Blob พิสูจน์ข้อนี้อยู่แล้ว)

**ถ้าวันหนึ่งมี swagger-ui/redoc:** enum design ป้องกันการลืมให้อยู่แล้ว — เพิ่ม `Slot::Docs`
เป็น compile error ที่ `Kind::slot()` และ `Slot::ALL`/`features()`/`shipped()`/`what()` ครบทุกจุด
**และน่าจะไม่ใช่ capability ที่สองด้วยซ้ำ** — มันคือ flag `--ui scalar|swagger|redoc` ที่ orthogonal
กับตัว capability แบบเดียวกับที่ `--orm` orthogonal กับ `postgres/mysql`
(spec ตัวเดียวกัน, `paths(...)` ตัวเดียวกัน, ต่างแค่ HTML ที่เสิร์ฟ) — เก็บไว้ตัดสินตอนมีคนขอ

**ผลข้างเคียงที่ต้องแก้ก่อน:** `main.rs:174` เรียก `env[0].0` ตรง ๆ
```rust
println!("Leave {} unset for the default adapter, as before.", env[0].0);
```
capability แรกที่ `env()` คืน slice ว่างคือตัวนี้ → **panic** ต้องแก้เป็น
`if let Some((first, _)) = env.first()` และข้ามบรรทัด `compose_up()` เมื่อไม่มี service ให้ boot
(~6 บรรทัดใน `main.rs`) นี่คือหนี้เดิมที่ capability ที่ไม่มี env จะเป็นตัวเปิด

---

## 3. Decision: สิ่งที่ gen

### 3.1 ไฟล์ใหม่ไฟล์เดียว: `src/api/openapi.rs`

```rust
//! OpenAPI document + สอง route ที่เสิร์ฟมัน — เขียนโดย `kodegen add scalar`
use axum::Router;

#[cfg(feature = "apidocs")]
#[derive(utoipa::OpenApi)]
#[openapi(paths(
    crate::api::health,
    crate::api::user::create_user,
    crate::api::user::get_user,
    crate::api::hello::say_hello,
    // kodegen:openapi:paths
))]
pub struct ApiDoc;

/// `/docs` (Scalar) และ `/openapi.json` — ว่างเปล่าเมื่อ `apidocs` ปิด
/// ซึ่งคือวิธีที่ production build ตัดทั้งสอง route ออกโดยไม่แตะ `router()`
pub fn routes() -> Router<super::Services> {
    #[cfg(feature = "apidocs")]
    {
        use utoipa::OpenApi;
        use utoipa_scalar::Servable;
        let spec = ApiDoc::openapi();
        let json = axum::Json(spec.clone());
        Router::from(utoipa_scalar::Scalar::with_url("/docs", spec)).route(
            "/openapi.json",
            axum::routing::get(move || {
                let json = json.clone();
                async move { json }
            }),
        )
    }
    #[cfg(not(feature = "apidocs"))]
    {
        Router::new()
    }
}
```

`cfg` อยู่**ในไฟล์นี้ไฟล์เดียว** ทำให้ `api/mod.rs` แก้ด้วย anchor เดิมได้ทั้งคู่:
`insert_pub_mod("openapi")` + `insert_before_line(".with_state(services)", "        .merge(openapi::routes())")`
— คือ edit ชุดเดียวกับที่ `kodegen feature` ทำอยู่แล้วเป๊ะ ไม่มีกลไกใหม่

> หมายเหตุต่อ `plan-persistence.md §4` ที่เขียนว่า "ไม่ใส่ `#[cfg(feature=...)]` ในโค้ดที่ generate":
> ข้อนั้นถูกผ่อนไปแล้วตั้งแต่ `MONGO_MOD`/`SEA_MOD`/`MSSQL_MOD` และที่นี่ cfg **ทำงานจริง** —
> มันคือ kill switch ของ production (§3.3) ไม่ใช่ ceremony

### 3.2 anchor-insert ในไฟล์เดิม (additive ล้วน)

| ไฟล์ | edit | helper ที่ใช้ |
|---|---|---|
| `src/api/mod.rs` | `pub mod openapi;` | `insert_pub_mod` |
| | `.merge(openapi::routes())` เหนือ `.with_state(services)` | `insert_before_line` |
| | `#[cfg_attr(feature="apidocs", derive(utoipa::ToSchema))]` เหนือ `pub struct ErrorRes {` | `insert_before_line` |
| | `#[cfg_attr(feature="apidocs", utoipa::path(...))]` เหนือ `async fn health()` | `insert_before_line` |
| `src/api/user.rs` | cfg_attr ToSchema เหนือ `pub struct CreateUserReq {` | `insert_before_line` |
| | cfg_attr path เหนือ `pub async fn create_user(` / `get_user(` | `insert_before_line` |
| `src/api/hello.rs` | cfg_attr path เหนือ `pub async fn say_hello(` | `insert_before_line` |
| `src/domain/model.rs` | cfg_attr ToSchema เหนือ `pub struct User {` | `insert_before_line` |
| `Cargo.toml` | `"apidocs"` เข้า `default = [...]` | กลไกเดิม |

แทรก attribute เหนือบรรทัด `pub struct X {` = ไปอยู่ระหว่าง `#[derive(Serialize)]` กับ `pub struct`
ซึ่ง Rust ยอมรับ (ลำดับ attribute ไม่มีความหมาย) — ทดสอบ compile แล้ว

`async fn health()` เป็น private fn: `#[utoipa::path]` สร้าง `__path_health` ที่ private เท่ากัน
แต่ `crate::api::openapi` เป็น child module จึงมองเห็น — **ทดสอบ compile แล้ว** ใช้ได้

### 3.3 Route + การ gate (ความเห็น)

**`/docs` + `/openapi.json` ทั้งคู่** — `/openapi.json` ไม่ได้แถม มันคือตัวที่ใช้ได้จริง
ตอนไม่มีเน็ต (§6.1), ตัวที่ contract test parse ได้ และตัวที่ client codegen ชี้มา

**production เปิด docs ดีไหม — ความเห็น: ไม่ควรเปิดโดยไม่ตั้งใจ แต่ default ต้องเปิด**
คนที่พิมพ์ `kodegen add scalar` แล้วรัน `cargo run` ต้องเห็น `/docs` ทันที ไม่งั้นคำสั่งนี้ไม่มีความหมาย

**กลไก gate = compile-time feature อย่างเดียว ไม่มี env**
- `default = ["sqlite", "apidocs"]` หลัง add → ปิดคือลบคำเดียวออกจาก `default` แล้ว build ใหม่
  โค้ดหายไปจริง ไม่ใช่แค่ route ที่ไม่ register — ตรงกับ state model ที่มี truth เดียวคือ `default = [...]`
- **ไม่ใส่ env kill switch โดยเจตนา**: มันคือ state ที่สองสำหรับการตัดสินใจเดียวกัน และ default
  ที่ปลอดภัยของ env จะเป็นแบบกลับด้าน (`APIDOCS=1` ถึงจะเปิด) ซึ่งทำให้ `add scalar` ไม่เห็นผลทันที
- ราคาที่ยอมรับ: คนที่ build ไบนารีเดียวส่งทุก environment จะได้ `/docs` ติดไปด้วย
  → README ต้องเขียนตรง ๆ ว่า "docs อยู่หลัง ingress/auth หรือ build โดยไม่มี `apidocs`" (ความเสี่ยงข้อ §6.2)

---

## 4. Decision: interaction กับ `kodegen feature` — จุดที่ยากที่สุด

### กติกา: **`feature` annotate เสมอ ผ่าน `cfg_attr` / `add scalar` ค่อยไปเก็บชื่อเอง**

สองทิศทางต้องทำงานทั้งคู่ และแต่ละทิศใช้กลไกละหนึ่งอย่าง:

**ทิศที่ 1 — `feature order` แล้วค่อย `add scalar`**

`API_TPL` และ `DOMAIN_TPL` ใน `scaffold.rs` ใส่ annotation **เสมอ** ไม่สนใจว่าโปรเจกต์เปิด `apidocs` ไหม:

```rust
#[cfg_attr(feature = "apidocs", derive(utoipa::ToSchema))]
pub struct Create{{Feature}}Req { ... }

#[cfg_attr(feature = "apidocs", utoipa::path(
    post, path = "/{{feature}}", tag = "{{feature}}",
    request_body = Create{{Feature}}Req,
    responses(
        (status = 201, description = "created"),
        (status = 400, description = "validation failed", body = ErrorRes),
    ),
))]
pub async fn create_{{feature}}(...)
```

เมื่อ `apidocs` ปิด `cfg_attr` หายไปทั้งบรรทัด → **ไม่ต้องมี utoipa ใน dependency graph เลย**
ทดสอบแล้ว: build ผ่านทั้ง `default = ["sqlite"]` และ `default = ["sqlite","apidocs"]`, clippy สะอาดทั้งสอง

จากนั้น `add scalar` **ค้นหา** handler ที่ annotate ไว้แล้ว เพื่อสร้าง `paths(...)`:
สแกน `src/api/*.rs` (ยกเว้น `*_grpc.rs`) จากเนื้อไฟล์ *ที่วางแผนไว้ใน memory* (หลัง edit ของตัวเอง)
— เจอบรรทัดที่มี `utoipa::path(` แล้วมองหา `async fn <name>(` ตัวถัดไป → ได้ `crate::api::<module>::<name>`
ประมาณ 15 บรรทัด ไม่ต้องใช้ `syn` และซื่อสัตย์โดยนิยาม: **อะไรที่ annotate ไว้ คือสิ่งที่อยู่ใน spec**

**ทิศที่ 2 — `add scalar` แล้วค่อย `feature order`**

`add_feature` เพิ่มเงื่อนไขเดียว:

```rust
if project.join("src/api/openapi.rs").is_file() {
    edits.push(edit(project, "src/api/openapi.rs", |c| {
        insert_before_line(&c, "// kodegen:openapi:paths",
            &format!("    crate::api::{name}::create_{name},\n    crate::api::{name}::get_{name},"))
    })?);
}
```

state-awareness ทั้งหมดของแผนนี้คือ `is_file()` หนึ่งครั้ง — ไม่ใช่ decision tree แบบ `Db`/`Mongo`
ที่ต้องอ่าน `bootstrap.rs` มาเดา type

**`--transport grpc`:** ไม่แตะเลย OpenAPI ไม่ได้ describe gRPC และ `api/<f>_grpc.rs` ไม่มี HTTP route
ให้ document — scan ข้าม `*_grpc.rs` และ `add_feature` ทำเงื่อนไขข้างบนเฉพาะ `Transport::Rest`

---

## 5. Decision: Testing / CI

### 5.1 unit test ของ kodegen

- ไฟล์ที่ `add scalar` วางแผนจะเขียน ตรงกับ dry-run (pattern เดิม)
- `add scalar` ซ้ำ → error
- **path discovery**: ป้อนเนื้อไฟล์ที่มี/ไม่มี annotation → ได้ list ที่ถูก และไม่หลง `*_grpc.rs`
- **`feature` หลัง `add scalar`** แทรกสองบรรทัดเหนือ marker และ marker ยังอยู่หลังรันซ้ำ
  (คู่กับเทสต์ `markers_take_precedence_and_survive_repeat_runs` ที่มีอยู่)
- **`feature` โดยไม่มี `openapi.rs`** ต้องไม่พยายามแก้ไฟล์ที่ไม่มี

### 5.2 contract test ในโปรเจกต์ที่ generate — `tests/apidocs.rs`

```
GET /openapi.json  -> 200, parse เป็น JSON ได้, มี paths ครบทุก route ที่รู้จัก
GET /docs          -> 200
```
ห้ามแตะ CDN — เทสต์ยืนยัน HTML ตอบ 200 เท่านั้น ไม่ได้ยืนยันว่า JS โหลดได้

**"spec ครอบทุก route ที่ scaffold" ตรวจอัตโนมัติในโปรเจกต์ไม่ได้** — axum 0.8 ไม่ export
รายการ route ออกจาก `Router` จึงไม่มีทางเทียบ router ↔ spec ในตัวมันเอง
→ ตรวจฝั่ง kodegen แทน (§5.3) และเขียนข้อจำกัดนี้ลง README ตรง ๆ

### 5.3 CI row ใหม่ — **จำเป็น หนึ่งแถว**

เหตุผลเดียวกับทุกแถวในไฟล์นั้น: annotation ทั้งหมดอยู่ใน string constant `cargo test` ของ kodegen
ไม่เคยเอา rustc ไปจ่อดูสักบรรทัด และแถวนี้ตรวจของที่ไม่มีแถวอื่นตรวจ — **ลำดับสองทิศทางของ §4**

```yaml
- capability: scalar
```
โดย step ที่มีอยู่แล้วครอบให้เกือบหมด (`new` → `feature order` → `add scalar` → `feature notification`
→ fmt/clippy/test) เพราะแถวอื่นทำลำดับนี้อยู่แล้ว **บวก step ใหม่หนึ่งอัน**:

```yaml
- name: The spec must cover both scaffold orders
  working-directory: ${{ runner.temp }}/demo
  run: |
    cargo run --locked --quiet --bin ... &   # หรือใช้ tests/apidocs.rs ที่ generate มาแล้ว
    # assert /openapi.json มี /order (feature ก่อน add) และ /notification (feature หลัง add)
```
ทำง่ายกว่านั้น: ให้ `tests/apidocs.rs` อ่าน spec แล้ว `println!` path ทั้งหมด แล้ว CI `grep`
— ไม่ต้อง boot server จริง เพราะ `bootstrap::build_router()` + `tower::ServiceExt::oneshot`
ทำงานใน process เดียวอยู่แล้ว (แบบเดียวกับ `tests/api.rs`)

ไม่ต้องเพิ่ม `generated-all` — `apidocs` ไม่มี server ให้ boot และไม่ชนกับ feature ใด
แต่ **ควรใส่ `scalar` เข้า `new demo-all --with ...`** เพื่อพิสูจน์ feature unification ตอนเปิดทุกอย่างพร้อมกัน

---

## 6. ความเสี่ยง / สิ่งที่ต้องเฝ้า

1. **`/docs` ต้องมีเน็ต** — `utoipa-scalar` ฝัง spec ลง HTML (1961 bytes) แต่ `<script>` ชี้ไป
   `https://cdn.jsdelivr.net/npm/@scalar/api-reference` **ตรวจซอร์สแล้ว ไม่ใช่การเดา**
   ขัดกับสัญญา "no-server default" ของ template อยู่ครึ่งหนึ่ง
   - ครึ่งที่ยังจริง: `/openapi.json` offline ได้ 100%, `cargo test` ไม่แตะเน็ต, build ไม่แตะเน็ต
   - ครึ่งที่ไม่จริง: หน้า `/docs` เป็นหน้าขาวถ้าไม่มีเน็ต
   - **ทางแก้ที่ไม่ทำตอนนี้**: `Scalar::custom_html()` + vendor bundle ของ `@scalar/api-reference`
     (~1.5 MB) เข้า template → ไบนารี kodegen โต และทุก repo ที่ generate โตตาม
     จ่ายเมื่อมีคนบอกว่าต้องใช้ offline จริง ไม่ใช่ก่อนหน้านั้น
   - **ต้องเขียนใน README ตรง ๆ** ไม่ใช่ให้ผู้ใช้ไปเจอเอง
2. **`/docs` ติดไปกับ production build** ถ้าคนไม่ลบ `apidocs` ออกจาก `default` — เปิดเผย
   API surface ทั้งหมด ทางลด: README + บรรทัดที่ `add` พิมพ์ออกมาตอนจบต้องบอกวิธีปิด
3. **path string ซ้ำสองที่** (`.route(...)` กับ `path = "..."`) — utoipa ตรวจให้ไม่ได้
   scaffolded code drift ไม่ได้เพราะ generate จาก `{{feature}}` ตัวเดียว แต่คนแก้ route มือแล้วลืมแก้ path
   จะได้ spec ที่ชี้ผิดโดยไม่มีใครเตือน → ถ้าเจ็บจริงคือ signal ให้ย้ายไป `utoipa-axum` (§1.1)
   ซึ่งตอนนั้นต้องยอมให้ `API_TPL` state-aware
4. **route ที่เขียนมือแล้วไม่ annotate จะหายจาก spec เงียบ ๆ** — เป็นธรรมชาติของ derive-based
   ไม่มี compiler ตัวไหนจับได้ เขียนลง README ว่า "route ใหม่ = `#[cfg_attr(..., utoipa::path(...))]`
   + หนึ่งบรรทัดใน `paths(...)`" เหมือนที่ README มี checklist "Adding a Feature" อยู่แล้ว
5. **utoipa major bump** — annotation กระจายอยู่ใน string constant สองไฟล์ (`capability.rs`, `scaffold.rs`)
   การขึ้น 6.0 คือแก้ทั้งสองที่พร้อมกัน; MSRV ไม่ใช่ปัญหา (1.75 vs 1.88 มีที่ว่างเยอะ)
6. **`main.rs` `env[0]` panic** (§2) — ต้องแก้ก่อน ไม่ใช่หลัง

---

## 7. หลักฐานที่วัดจริง

ทำใน scratchpad ด้วย `templates/hexagonal/` ตัวจริง (copy) ไม่ได้ทำใน crate เปล่า:

| การวัด | ผล |
|---|---|
| lock ที่ commit ไว้ / regenerate เปล่า / + utoipa | 482 / **484** / **487** → marginal **+3 packages** (`utoipa`, `utoipa-gen`, `utoipa-scalar`) |
| `cargo build --locked` (`default = ["sqlite","apidocs"]`) | ผ่าน |
| `cargo build --locked` (`default = ["sqlite"]`, ไฟล์ annotate ไว้แล้ว) | **ผ่าน** — `cfg_attr` ลบทิ้งครบ |
| `cargo clippy --all-targets` ทั้งสองสถานะ | สะอาดทั้งคู่ |
| `paths` ใน spec | `["/health", "/hello/{name}", "/users", "/users/{id}"]` — ครบทุก REST route ของ template |
| `components.schemas` (ไม่ได้ประกาศ) | `["CreateUserReq", "ErrorRes", "User"]` — auto-collection ทำงาน |
| `info` (ไม่ได้ประกาศ) | `{"title":"hexagonal-rs","version":"0.1.0"}` — มาจาก Cargo.toml |
| `GET /docs` | 200, **1961 bytes**, มี `cdn.jsdelivr.net` |
| annotate private `async fn health()` | compile ผ่าน (child module เห็น `__path_health`) |

---

## 8. ขนาด + สิ่งที่จงใจไม่ทำ

### ขนาด: **M**

| ไฟล์ | งาน |
|---|---|
| `src/capability.rs` | `Capability::Scalar`, `Kind::ApiDocs`, `feature()`, `env() -> &[]`, `list()` entry, `add_scalar()` + template constants + path scanner + tests — **ก้อนใหญ่สุด ~200 บรรทัดรวม comment** |
| `src/main.rs` | กัน `env[0]` panic + ข้าม `compose_up` เมื่อไม่มี service (~6 บรรทัด) |
| `src/scaffold.rs` | `API_TPL`/`DOMAIN_TPL` เพิ่ม cfg_attr, `add_feature` เพิ่มเงื่อนไข `openapi.rs` (~25 บรรทัด) |
| `templates/hexagonal/Cargo.toml` | 2 optional deps + feature `apidocs` |
| `templates/hexagonal/Cargo.lock` | regenerate (+3) |
| `.github/workflows/ci.yml` | 1 แถว + 1 step, และ `scalar` เข้า `demo-all` |
| README ×2 | capability ใหม่ + คำเตือน CDN + คำเตือน production |

เทียบกับของที่ ship แล้ว: ใหญ่กว่า `redis` (S) เพราะแตะ `scaffold.rs` ด้วย, เล็กกว่า `mongodb` (M)
เพราะไม่มี adapter ไม่มี port ไม่มี compose ไม่มี env ไม่มี slot conflict ไม่มี refusal path

### สิ่งที่จงใจไม่ทำ

- ❌ **`utoipa-axum` / `OpenApiRouter`** — ทำให้ `API_TPL` state-aware (§1.1) แลกกับการลบ path ซ้ำ
  ยังไม่คุ้ม จะทบทวนเมื่อความเสี่ยง §6.3 เกิดจริง
- ❌ **swagger-ui / redoc / rapidoc / `Slot::Docs`** — slot ที่มี occupant เดียวคือกติกาที่ไม่มีอะไรให้ห้าม
  และถ้ามาจริงน่าจะมาเป็น flag `--ui` (§2)
- ❌ **vendor Scalar JS bundle** — ~1.5 MB เข้าไบนารีและทุก repo เพื่อแก้ปัญหาที่ยังไม่มีคนบ่น (§6.1)
- ❌ **env kill switch ของ `/docs`** — state ที่สองสำหรับการตัดสินใจเดียวกัน (§3.3)
- ❌ **auth / rate limit หน้า `/docs`** — เป็นงานของ ingress ไม่ใช่ของ template
- ❌ **OpenAPI สำหรับ `--transport grpc`** — ผิด protocol; ถ้าต้องการจริงคือ gRPC reflection
  ซึ่งเป็น capability คนละตัว
- ❌ **`cargo run --bin gen-openapi > openapi.json`** สำหรับ client codegen — `curl localhost:8080/openapi.json`
  ทำได้แล้ว เพิ่มเมื่อมีคนต้องการ spec ตอน build time โดยไม่ยอม boot process
- ❌ **customize `info` / `servers` / `security` scheme** — `#[derive(OpenApi)]` เติม title/version
  จาก Cargo.toml ให้แล้ว ที่เหลือคือคนแก้ไฟล์ `openapi.rs` เอง ซึ่ง additive อยู่แล้ว
- ❌ **`kodegen remove scalar`** — kodegen ยังไม่มี `remove` สำหรับ capability ใดเลย ไม่เริ่มที่ตัวนี้
