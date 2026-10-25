-- OPet 存档 schema · 设计书 §3.4 的 12 表，逐字对照 + 下面【偏离】注释里那三处有意改动。
--
-- ⚠️ 本文件由 `include_str!` 嵌进二进制（不依赖运行时文件，`opet pull` 在任意 cwd 都能跑）。
-- 改任何一列都必须同步升 `migrate::SCHEMA_VERSION` 并加一条 ALTER 迁移，不许就地改历史版本。
--
-- 【偏离 1】新增 `profiles` 表 + `gacha_pull.profile_id` 列。
--   §3.4 里 `equipped.profile_id` 引用了一个并不存在的 profiles 表，而 §10.2 的
--   `opet audit verify --account local` 需要一个账号维度。v1 只有 'local' 一行，
--   但列先留好 ⇒ M7 多档案时不用再迁移一次（迁移一次就要面对一次"旧库升不上来"）。
--
-- 【偏离 2】`gacha_pull` 比 §3.4 多 6 列：item_kind / batch_index / dice_used /
--   is_duplicate / shards_gained / ssr_prob。
--   理由不是"想多存点"：`audit::AuditRecord::canonical()` 把这 6 个字段全都算进摘要，
--   少存任何一列，`opet audit verify` 就无法从库里重建记录 ⇒ 审计链在库里断掉，
--   §6.1"审计重放可校验任意记录"这条门禁直接作废。`pity_counter` 存的是 `pity_before`。
--
-- 【偏离 3】`gacha_banner` 比 §3.4 多 5 列：soft_step / batch_size / ten_pull_sr_floor /
--   dup_shards / ssr_redeem_cost。§3.4 只存了 `base_rate/soft_pity/hard_pity`，但决定
--   **综合概率**的量一共有 8 个（`GachaParams` 的全部字段）。少存任何一个，
--   `audit verify` 就只能相信"代码里那个常数没被人改过"——而"公示 == 代码"恰恰不能靠相信。
--   ⇒ 凡是进过骰子与折算的数字都必须能在库里读回来（计划"列少了就补 DDL，不许代码里藏魔数"）。
--   带 DEFAULT，所以旧库/手工插的行仍然可解析。
--
-- 【偏离 4】无。`gacha_pull`/`inventory`/`content_packs`/`equipped` 的主键与 CHECK 一字未改，
--   特别是 `gacha_banner.currency` 的 CHECK —— 它是硬约束 #1（随机 ≠ 付费）在 schema 层的落点，
--   `assert_no_paid_randomness` 的第一层。动它之前先读 §3.11.4。

PRAGMA foreign_keys = ON;

CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL); -- schema_version, seed, audit_head, audit_len

CREATE TABLE profiles (
  id TEXT PRIMARY KEY,            -- v1 只有 'local'
  display_name TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE pets (
  id TEXT PRIMARY KEY,            -- 'cat_shiro'
  name TEXT NOT NULL, locale_key TEXT,
  rarity TEXT NOT NULL CHECK (rarity IN ('N','R','SR','SSR')),
  tags TEXT,                      -- JSON array: ['round','sleepy']
  atlas_path TEXT,                -- assets/pets/cat_shiro/v3/sheet.webp
  manifest_path TEXT NOT NULL,    -- assets/pets/cat_shiro/v3/pet.json
  scale REAL DEFAULT 1.0, shadow TEXT, created_at INTEGER
);
CREATE TABLE accessories (
  id TEXT PRIMARY KEY, name TEXT NOT NULL,
  slot TEXT NOT NULL CHECK (slot IN ('head','face','neck','back','hand')),
  rarity TEXT NOT NULL CHECK (rarity IN ('N','R','SR','SSR')),
  anchor_offset TEXT,             -- {"head":[0,-34]}
  z_order INTEGER DEFAULT 10, palette TEXT, manifest_path TEXT NOT NULL
);
CREATE TABLE inventory (
  kind TEXT NOT NULL CHECK (kind IN ('pet','accessory')), item_id TEXT NOT NULL,
  count INTEGER NOT NULL DEFAULT 1, duplicates INTEGER NOT NULL DEFAULT 0, -- 重复转碎片来源
  obtained_at INTEGER, obtained_from TEXT,                                 -- 'gacha:cat_series#137'
  PRIMARY KEY (kind, item_id)
);
CREATE TABLE shards (currency TEXT PRIMARY KEY, amount INTEGER NOT NULL);
CREATE TABLE equipped (profile_id TEXT, slot TEXT, item_id TEXT, PRIMARY KEY (profile_id, slot));

-- === 商业化钩子（v1 建表 + 读写路径，但不接支付；D2/M7 时只需接 webhook）===
-- ⚠️ 本表**没有** rarity / weight 列，且 `ContentPack` 结构体 `deny_unknown_fields`：
--    付费内容包在数据结构层面就不可能携带随机抽取权重（§3.11.4 第 2 条）。
CREATE TABLE content_packs (
  id TEXT PRIMARY KEY,              -- 'cat_series_01'
  title TEXT NOT NULL, kind TEXT CHECK (kind IN ('pets','accessories','bundle')),
  price_cents INTEGER, currency_code TEXT,   -- 明码标价，随机抽取不得使用（见 §3.11.4）
  store TEXT CHECK (store IN ('itch','steam','mor','free')),
  pack_path TEXT NOT NULL,          -- packs/cat_series_01.petpack
  signature TEXT, release_notes TEXT, is_dlc INTEGER DEFAULT 1
);
CREATE TABLE entitlements (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  pack_id TEXT REFERENCES content_packs(id),
  source TEXT NOT NULL,             -- 'purchase:itch#order' | 'redeem:code' | 'free' | 'gacha'
  status TEXT CHECK (status IN ('active','revoked','expired')),   -- refund → revoked
  external_ref TEXT,                -- 订单号，不落任何支付凭证明细
  acquired_at INTEGER, last_sync_at INTEGER
);
CREATE TABLE license_state (
  key TEXT PRIMARY KEY, value TEXT  -- mor_endpoint(可换), entitlement_cache_sig, last_offline_grace_ts
);

CREATE TABLE gacha_banner (
  id TEXT PRIMARY KEY, series TEXT, start_at INTEGER, end_at INTEGER,
  base_rate TEXT NOT NULL, -- JSON: {"N":0.60,"R":0.28,"SR":0.10,"SSR":0.02}
  soft_pity INTEGER NOT NULL,      -- = GachaParams.soft_start（第几抽起抬 SSR）
  hard_pity INTEGER NOT NULL,
  soft_step INTEGER NOT NULL DEFAULT 60000,          -- 每抽抬升的百万分位（= +6%）
  batch_size INTEGER NOT NULL DEFAULT 10,            -- 十连
  ten_pull_sr_floor INTEGER NOT NULL DEFAULT 1 CHECK (ten_pull_sr_floor IN (0,1)),
  dup_shards TEXT NOT NULL DEFAULT '[2,8,25,100]',   -- 按 N/R/SR/SSR 顺序
  ssr_redeem_cost INTEGER NOT NULL DEFAULT 120,      -- 碎片自选 SSR
  currency TEXT NOT NULL DEFAULT 'companion_time'
    CHECK (currency IN ('companion_time','shards'))   -- ← 结构上禁止真钱抽盲盒
);
CREATE TABLE gacha_pull (
  seq INTEGER PRIMARY KEY AUTOINCREMENT,
  profile_id TEXT NOT NULL REFERENCES profiles(id),
  banner_id TEXT NOT NULL REFERENCES gacha_banner(id),
  ts INTEGER NOT NULL,
  rarity TEXT NOT NULL CHECK (rarity IN ('N','R','SR','SSR')),
  item_kind TEXT NOT NULL CHECK (item_kind IN ('pet','accessory')),
  item_id TEXT NOT NULL,
  pity_counter INTEGER NOT NULL,       -- = AuditRecord.pity_before（本 pity 窗口已抽次数）
  batch_index INTEGER NOT NULL,        -- 十连内第几抽（0-indexed）
  dice_used INTEGER NOT NULL,          -- 本抽掷了几次骰
  is_duplicate INTEGER NOT NULL DEFAULT 0 CHECK (is_duplicate IN (0,1)),
  shards_gained INTEGER NOT NULL DEFAULT 0,
  ssr_prob INTEGER NOT NULL,           -- 百万分位；审计要能核对**保底数学**，不只是核对骰子
  rng_state_hash TEXT NOT NULL,        -- sha256(RNG_DOMAIN ‖ seed ‖ seq)：掷骰**前**的挑战值
  audit_prev_hash TEXT NOT NULL        -- 上一条的 digest（创世 = 64 个 '0'）
);
CREATE INDEX idx_pull_account ON gacha_pull(profile_id, banner_id, seq);

CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT, scope TEXT DEFAULT 'global');
CREATE TABLE session_log (id INTEGER PRIMARY KEY AUTOINCREMENT, ts INTEGER, event TEXT, payload TEXT);
