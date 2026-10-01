// 守设计里最硬的一条约束：**余额只有一处算法**。
//
// docs/客户管理模块设计.md §3.2① 要求：客户视图的所有余额必须走
// `settle::open_entries`，不许自己写 SQL 算。为什��要机器守：
//   · 「约定」靠人记住 = 迟早有人在自己模块里写一句 SUM(...)
//   · 而一旦出现第二个算法，「为什么这个客户欠款和往来核销页不一样」
//     就是最难查的一类问题 —— 两边都对，只是口径不同。
//
// 拦的形状：
//   1. customers.rs 里不许出现 voucher_entry / stock_move / settle_record 的裸 SQL
//   2. 不许出现 SUM( / GROUP BY 这类聚合（余额是一个个 OpenEntry 加出来的，
//      不是 SQL 聚合出来的 —— SQL 聚合会绕过 H-3 的 posted 过滤）
//   3. 反过来：**必须**出现 settle::open_entries（用了别的方式也算破例）
const fs = require("fs");
const path = require("path");

const ROOT = path.join(__dirname, "..");
const F = "crates/findb/src/customers.rs";
const p = path.join(ROOT, F);
if (!fs.existsSync(p)) {
  console.log("FAIL 找不到 " + F + " —— 守门人自己坏了（文件被改名/删除？）");
  process.exit(1);
}
const src = fs.readFileSync(p, "utf8");
const lines = src.split(/\r?\n/);

// 只看非测试代码（测试里当然可以查表）
const tIdx = lines.findIndex((l) => /^#\[cfg\(test\)\]/.test(l));
const bodyLines = (tIdx >= 0 ? lines.slice(0, tIdx) : lines).map((l, i) => [i + 1, l]);

let bad = 0;
const inComment = (l) => {
  const t = l.trim();
  return t.startsWith("//") || t.startsWith("*") || t.startsWith("/*");
};

const FORBID = [
  {
    name: "直接查 voucher_entry（余额必须走 open_entries）",
    re: /\bFROM\s+voucher_entry\b/i,
    why: "自己查分录 = 绕过 open_entries 的 H-3 过滤（只认已记账）",
  },
  {
    name: "直接查 stock_move / settle_record",
    re: /\bFROM\s+(stock_move|settle_record)\b/i,
    why: "往来余额与库存无关；出现它说明算错了口径",
  },
  {
    name: "SQL 聚合",
    re: /\bSUM\s*\(|\bGROUP\s+BY\b/i,
    why: "余额要逐条 OpenEntry 相加（H-3 在 open_entries 里）；SQL 聚合会绕过 posted 过滤",
  },
];

for (const f of FORBID) {
  let hit = 0;
  for (const [no, l] of bodyLines) {
    if (inComment(l)) continue;
    if (f.re.test(l)) {
      hit++;
      console.log(`FAIL ${F}:${no}  ${f.name}`);
      console.log(`       ${l.trim().slice(0, 100)}`);
      console.log(`       ${f.why}`);
      bad++;
    }
  }
  if (hit === 0) console.log(`OK   没有${f.name}`);
}

// 正向要求：**取余额的入口**必须调 settle::open_entries。
//
// 注意判据的粒度：只看「文件里出现过 open_entries」是不够的 ——
// `list()` 里也调它，所以把 `open_lines` 里的调用换成自研函数时，
// 「出现过」依然为真，检查就漏了。变异自检的 ③ 就是这么漏过去的。
// 所以这里必须按**函数**判定：`open_lines`（余额的唯一取数口）里必须有它。
const openLinesStart = lines.findIndex((l) => /^pub fn open_lines\(/.test(l));
if (openLinesStart < 0) {
  console.log("FAIL " + F + " 里找不到 open_lines —— 它是余额的唯一取数口");
  process.exit(1);
}
let d = 0, openLinesEnd = -1;
for (let i = openLinesStart; i < lines.length; i++) {
  for (const ch of lines[i]) { if (ch === "{") d++; else if (ch === "}") d--; }
  if (d === 0 && i > openLinesStart) { openLinesEnd = i; break; }
}
if (openLinesEnd < 0) { console.log("FAIL 找不出 open_lines 的结尾"); process.exit(1); }
const openLinesBody = lines.slice(openLinesStart, openLinesEnd + 1);
const usesOpen = openLinesBody.some((l) => !inComment(l) && /settle::open_entries\s*\(/.test(l));
if (!usesOpen) {
  console.log(
    "FAIL " + F + " 的 open_lines 里没有 settle::open_entries —— " +
      "余额取数口换了实现。open_entries 是 H-3（只认已记账）的唯一落点，" +
      "换掉它余额就会把草稿算进去"
  );
  bad++;
} else {
  console.log(`OK   open_lines（第 ${openLinesStart + 1} 行）确实走 settle::open_entries（H-3 口径）`);
}

process.exit(bad === 0 ? 0 : 1);
