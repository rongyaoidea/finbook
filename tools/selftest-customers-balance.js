// 证明 check-customers-balance 真的会拦「自己写 SQL 算余额」。
//
// 三种破例都要被拦：
//   ① 直接查 voucher_entry
//   ② 用 SUM( ... ) 聚合
//   ③ 完全不用 open_entries（换成别的方式）—— 只靠前两条守不住
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");

const ROOT = path.join(__dirname, "..");
const F = path.join(ROOT, "crates", "findb", "src", "customers.rs");
const C = path.join(ROOT, "tools", "check-customers-balance.js");
const orig = fs.readFileSync(F, "utf8");

function run() {
  try {
    const o = execFileSync(process.execPath, [C], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
    return { ok: true, out: o };
  } catch (e) {
    return { ok: false, out: String((e && e.stdout) || "") + String((e && e.stderr) || "") };
  }
}

console.log("=== 基线 ===");
const b = run();
console.log(b.ok ? "OK   当前实现通过" : "FAIL 基线就红：\n" + b.out);
if (!b.ok) process.exit(1);

// 找一个稳定锚点：open_lines 函数体
const ANCHOR = "    let all = settle::open_entries(db, AR_ACCOUNT, period, false)?;";
if (orig.indexOf(ANCHOR) < 0) { console.log("FAIL 锚点找不到"); process.exit(1); }

const MUTS = [
  [
    "① 自己查 voucher_entry",
    `    let mut all = db.conn().prepare(
        "SELECT aux_key, SUM(debit) - SUM(credit) FROM voucher_entry WHERE account_code LIKE '1122%'"
    )?;
    let _ = all;`,
  ],
  [
    "② 用 SQL 聚合算余额",
    `    let _sum: String = db.conn().query_row(
        "SELECT SUM(debit) - SUM(credit) FROM voucher_entry WHERE account_code='1122'",
        [],
        |r| r.get(0),
    )?;`,
  ],
  [
    "③ 完全不用 open_entries（自己写个函数名）",
    `    let all = my_own_balance_query(db, AR_ACCOUNT, period)?;`,
  ],
];

let allGood = true;
for (const [name, injected] of MUTS) {
  const mutated = orig.replace(ANCHOR, injected);
  if (mutated === orig) { console.log("FAIL 变异没写进去：" + name); allGood = false; continue; }
  fs.writeFileSync(F, mutated, "utf8");
  const r = run();
  const bad = !r.ok;
  console.log(
    (bad ? "PASS 拦下  " : "FAIL 漏过  ") + name + "  → 检查" + (bad ? "报红" : "仍然通过")
  );
  if (!bad) console.log("       " + r.out.trim().slice(0, 160));
  fs.writeFileSync(F, orig, "utf8");
  if (!bad) allGood = false;
}

const after = run();
console.log("\n还原后基线：" + (after.ok ? "OK（绿）" : "FAIL（仍红）"));
const pass = allGood && after.ok;
console.log(pass ? "\n全部 3 个变异都被拦下，文件已还原" : "\n有变异漏过");
process.exit(pass ? 0 : 1);
