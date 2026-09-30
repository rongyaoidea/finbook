// 给 check-design-doc.js 加自检：证明它真的会拦，而不是永真通过。
//
// 「检查通过」不等于「检查有用」。三���探针对应三种失效：
//   1. 行号漂移（代码挪了一行，文档没跟）—— 最常见的失效
//   2. 文档删掉约束声明（「不做信用管理」这行被删了）
//   3. 文档引用了一个没登记的位置（等于「不受校验的引用」）
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");

const ROOT = "C:\\Users\\Administrator\\Documents\\Default Project\\finbook";
const CHECK = path.join(ROOT, "tools", "check-design-doc.js");
const DOC = path.join(ROOT, "docs", "客户管理模块设计.md");
const SCM = path.join(ROOT, "crates", "findb", "src", "scm.rs");

const docOrig = fs.readFileSync(DOC, "utf8");
const scmOrig = fs.readFileSync(SCM, "utf8");

function run() {
  try {
    execFileSync(process.execPath, [CHECK], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
    return { ok: true, out: "" };
  } catch (e) {
    return { ok: false, out: String((e && e.stdout) || "") + String((e && e.stderr) || "") };
  }
}

function probe(name, apply, restore, mustSee) {
  apply();
  const r = run();
  restore();
  const good = !r.ok && mustSee.test(r.out);
  console.log(
    (good ? "PASS 拦下  " : "FAIL 漏过  ") + name +
    (good ? "" : "  （退出=" + (r.ok ? "0" : "非0") + "，输出里找不到 " + mustSee + "）")
  );
  if (!good && r.out) console.log("       " + r.out.split(/\r?\n/).filter((l) => /FAIL/.test(l)).slice(0, 3).join("\n       "));
  return good;
}

let all = true;

// 先确认基线是绿的（否则下面的探针没有意义 —— 基线红着的话「拦下」是假的）
const base = run();
console.log((base.ok ? "OK   基线通过" : "FAIL 基线就不通过，下面探针无意义"));
if (!base.ok) { console.log(base.out); process.exit(1); }

// 1. 行号漂移：在 scm.rs 的信用检查那一行**上面**插一行注释 → 真实代码挪到 1162
all &= probe(
  "代码漂移：scm.rs 在信用检查上方插一行，文档行号失效",
  () => {
    const s = scmOrig.split(/\r?\n/);
    s.splice(1160, 0, "// 漂移探针：这行注释会让真实代码下移一行");
    fs.writeFileSync(SCM, s.join("\r\n"), "utf8");
  },
  () => fs.writeFileSync(SCM, scmOrig, "utf8"),
  /scm\.rs:1161 不再匹配/
);

// 2. 约束声明被删
all &= probe(
  "约束被删：文档里去掉「不做信用管理」",
  () => fs.writeFileSync(DOC, docOrig.replace(/不做信用管理/g, "信用管理"), "utf8"),
  () => fs.writeFileSync(DOC, docOrig, "utf8"),
  /文档缺约束声明/
);

// 3. 未登记的引用
all &= probe(
  "未登记引用：文档里加一个 scm.rs:99999",
  () => fs.writeFileSync(DOC, docOrig + "\n\n见 `scm.rs:99999`。\n", "utf8"),
  () => fs.writeFileSync(DOC, docOrig, "utf8"),
  /没有登记到本检查里/
);

// 还原确认
const after = run();
console.log("\n还原后基线：" + (after.ok ? "OK（绿）" : "FAIL（仍红，探针自己破坏了文件）"));
if (!after.ok) console.log(after.out);

const pass = all && after.ok;
console.log(pass ? "\n全部 3 个探针都生效且都被拦下，文件已还原" : "\n有探针没生效");
process.exit(pass ? 0 : 1);
