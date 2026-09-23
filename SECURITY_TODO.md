# FinBook 安全加固待办事项

> 本文件反映**当前代码真实状态**（2026-09 核实，逐项带代码证据）。
> 结论：20 项历史修复中 18 项已核实完成、2 项部分完成；H-3/M-15 两项产品决策已定案并落地。

## 一、已完成并核实（18 项）

### P0 级 ✅

| ID | 问题 | 证据 |
|----|------|------|
| CRIT-1 | 默认监听 127.0.0.1 | `crates/finweb/src/main.rs:35`（`FINBOOK_LISTEN` 默认 `127.0.0.1:8080`） |
| HIGH-1 | Web 层落地 DataScope | `crates/finweb/src/handlers.rs:1579` `with_data_scope`、`:1604` `retain(can_see_voucher)`、`:1621` get 分支 403 |
| HIGH-2 | voucher_unaudit 权限校验 | `crates/finweb/src/handlers.rs:1853` `require(Perm::VoucherUnaudit)` |
| HIGH-3 | 凭证删除守卫 | `crates/finweb/src/handlers.rs:1999-2002` 已记账 400；`:1995` 已审核拦截；`:2005` 结账期间拦截 |
| HIGH-4 | prepared_by 统一 username | `crates/finweb/src/handlers.rs:1730`；桌面 `crates/finui/src/views/voucher_edit.rs:484` |
| HIGH-5 | DataScope 默认收紧 | `crates/fincore/src/user.rs:336` `own_voucher_only = role != Admin`；建号 `handlers.rs:901-903` 强制置 true |
| HIGH-6 | must_change_pwd 服务端拦截 | `crates/finweb/src/state.rs:443-450`、`state.rs:575-582` 白名单外一律 401 |
| HIGH-7 | Cookie Secure 标志 | `crates/finweb/src/state.rs:717-728`（HttpOnly; SameSite=Lax，`FINWEB_SECURE_COOKIE=true` 时追加 Secure） |

### P1 级 ✅

| ID | 问题 | 证据 |
|----|------|------|
| H-C-1 | 资产负债表 `.neg()` 笔误 | `crates/fincore/src/report/balance_sheet.rs:45`、`:85`（已无 `.neg()`） |
| H-2 | 核销金额精度 | `crates/findb/src/settle.rs:187-202`（Rust 侧累加 + `money_param`，不 SQL SUM） |
| M-1 | 登录枚举消除 | `crates/finweb/src/realm.rs:195-222` 三种失败统一 `Ok(None)` + `burn_argon2`；`handlers.rs:496` 统一话术 |
| M-3 | 锁定窗口单位修复 | `crates/findb/src/security.rs:13-15` 分钟窗口；`state.rs:159/209` 秒 + `handlers.rs:474` `div_ceil(60)` |
| M-5 | 附件大小上限 + 白名单 | `crates/findb/src/attach.rs:20`（10MB）、`:23-28` 扩展名白名单、`:175-191` 强制校验；`handlers.rs:2089-2091` 二次校验 |
| M-6 | 附件路径穿越防护 | `crates/findb/src/attach.rs:209`（sha256 落盘名）、`:239-241` 拒 `..`、`:388-397` 导出名 sanitize；测试 `finweb/tests/api.rs:1174` |
| M-10 | parse_money Unicode | `crates/findb/src/imports.rs:322-334`（全角/负号归一，测试 `:957-964`）；`crates/fincore/src/money.rs:58-68` |
| M-14 | argon2 降级防护 | `crates/fincore/src/user.rs:459-466`（哈希失败即 panic，不降级） |
| M-2 | 登录失败 IP 限流 | `crates/finweb/src/state.rs:34,68`（50 次/IP 窗口）；`handlers.rs:481/494/503/442` 检查/记录/清除/取 IP |
| M-7 | 导入事务化 | `crates/findb/src/imports.rs:543→594`（begin）、`:653→729`（vouchers），整批同事务 |

## 二、产品决策已定案并落地 ✅

### H-3：余额口径 —— 定案 A（只算已记账）

- 科目余额表 / 三大报表 / 试算平衡 / 看板 = 期初余额 + **已记账分录**（`BalanceQuery` 默认 `posted_only=true`），草稿与已审核未记账不入余额。
- 账簿保留「只含已记账」开关（Web `#l-posted` 与桌面端默认勾选）；取消按「含未记账（排除作废）」查看，**行集与期初/滚动余额同口径**（修复原行集恒含非作废、快照按 posted 的打架 bug）。
- **例外**：期末结转类操作（结转损益 / 年末结转 / 预检 / 自动化清单展示）显式 `with_posted_only(false)`——结转凭证本身是草稿，靠含草稿口径判重防重复结转；结账前 checklist 要求全部记账，届时两种口径结果相同。
- 用例：`findb/balances.rs::balance_scope_h3_default_posted_only`、`ledger_rows_follow_posted_flag`；`finweb/tests/api.rs::trial_balance_default_posted_only_h3`。
- 文档：README §2.7/§2.8/§2.9/§2.10 + §5「余额口径（H-3 定案）」行。

### M-15：借贷平衡精度 —— 定案 A'（分位量化严格判平，不可配置）

- 分录金额**逐条量化到 2 位（分）**后借贷合计严格相等（`Voucher::balanced`），与落库 `money_param` 口径完全一致；差 0.005 的半分尾差不放过（否则入库后真差 0.01）。
- 试算平衡按 round2 判定，差额不足 1 分视为平衡。固定口径，不可配置。
- 用例：`fincore/voucher.rs::balanced_quantizes_per_entry`、`fincore/balance.rs::trial_balance_quantized_tolerance`；`finweb/tests/api.rs::unbalanced_voucher_rejected_m15`。
- 文档：README §5「借贷平衡（M-15 定案）」行。

## 三、部分完成（2 项）

| ID | 问题 | 现状 | 剩余工作 | 预估 |
|----|------|------|----------|------|
| M-9 | 未认证接口路径泄露 | **已修一半**：账套路径不外泄（`dto.rs:156` skip、`handlers.rs:390` 只进日志、`:425-427` 不返回 path）。缺：无统一认证中间件，未登录访问不存在的 `/api/*` 返 404、真实受保护路由返 401，仍可区分路径存在性 | 加路由层统一 401（或 404 归一） | 小 |
| L-2 | Period::from_ymm 异常值 | **部分缓解**：存在 `from_ymm_checked`（`fincore/src/period.rs:35`），Web 主入口已走 checked（`handlers.rs:1170/1179/2427`）。缺：桌面端 `finui/src/lib.rs:273`（及 `:566`）仍 unchecked | 桌面端换 checked | 小 |

## 四、待后续迭代

### 中优先级

| ID | 问题 | 现状 | 建议方案 | 预估 |
|----|------|------|----------|------|
| M-4 | 设备绑定可伪造 | 未完成：device_id 由客户端上报，可伪造换绑 | 首次绑定后禁改，服务端生成 device_hash | 中 |
| — | DataScope 未逐模块接入 | DataScope 默认收紧 + Web 凭证查询已接（HIGH-1/5），但其余模块查询未逐一走 scope | 逐模块排查接入 | 中 |
| L-1 | Money 除零返回 0 | 未完成：`fincore/src/money.rs:388/409/426` 除零静默返 0 | 改 Result 或 checked_div | 中（调用点多） |

### 低优先级

| ID | 问题 | 说明 |
|----|------|------|
| L-3~L-14 | 各种边界/死代码/风格 | 见原审查报告 |

## 五、推荐修复顺序

```
1. L-1 Money 除零改 Result（数据正确性隐患最大）
2. M-9 统一 401（补完一半的路径泄露修复）
3. L-2 桌面端换 from_ymm_checked
4. M-4 设备绑定强化
5. DataScope 逐模块接入 + L-3~L-14 渐进优化
```

## 六、CI 保障

- `.github/workflows/ci.yml` 已含 **cargo-audit** 任务（读 Cargo.lock，发现未修复 RUSTSEC 漏洞公告即失败）。
- 仓库 `.cargo/audit.toml` 记录 3 条**有据可依的忽略项**（lopdf 仅写不读、quick-xml 0.30 被 accesskit/zbus 上游锁死），并注明解除条件；其余公告一律拦截。
- 2026-09 依赖加固：rust_decimal 1.42→1.43（rkyv 0.7 可选边移出锁，RUSTSEC-2026-0235 消除）；calamine 0.26→0.36（quick-xml 0.31 移除，xlsx 导入攻击面修复，RUSTSEC-2026-0194/0195 可达实例消除）。
- 本地验证：fincore 99 / findb 172+21+7 / finweb 54 全部通过（2026-09）。
