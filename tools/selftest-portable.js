// 用这一轮**真实犯过的错**验证 check-portable：
// 它上一版就含 `C:\\Users\\Administrator\\Documents\\Default Project\\finbook`，
// 本地跑完全正常，只有 CI 的 Linux 上才崩。检查必须能抓到它。
// 注意：自检脚本自己也在这两个目录里，所以它**不许**含绝对路径
// —— 第一版写了 `const ROOT = "C:\\Users\\Administrator\\..."`，
// 结果 check-portable 把**自己**判成不可移植（第一次跑就红）。
// 一个检查脚本把自己也算进去是可接受的（它确实该守自己），
// 但前提是它自己先满足规则。
const fs = require("fs");
const path = require("path");
const { execFileSync } = require("child_process");

const ROOT = path.join(__dirname, "..");
const P = path.join(ROOT, "tools", "selftest-design-doc.js");
const C = path.join(ROOT, "tools", "check-portable.js");
const orig = fs.readFileSync(P, "utf8");

function run() {
  try {
    const o = execFileSync(process.execPath, [C], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
    return { ok: true, out: o };
  } catch (e) {
    return { ok: false, out: String((e && e.stdout) || "") + String((e && e.stderr) || "") };
  }
}

console.log("=== 基线（已修好）===");
const b = run();
console.log("  退出成功？ " + b.ok + (b.ok ? "（应为 true）" : "（不应红）"));

// 变异 1：把 ROOT 换回硬编码绝对路径（真实发生过的）
const mutated = orig.replace(
  'const ROOT = path.join(__dirname, "..");',
  'const ROOT = "C:\\\\Users\\\\Administrator\\\\Documents\\\\Default Project\\\\finbook";'
);
if (mutated === orig) { console.log("FAIL 变异没写进去（源码形态与预期不符）"); process.exit(1); }

console.log("\n=== 变异 1：ROOT 写回硬编码绝对路径 ===");
fs.writeFileSync(P, mutated, "utf8");
const r1 = run();
console.log("  退出成功？ " + r1.ok + "   ← 必须 false");
console.log(r1.out.split(/\r?\n/).filter((l) => /FAIL|修法/.test(l)).map((l) => "  " + l).join("\n"));

// 变异 2：写死 CRLF（另一个真实形状）
console.log("\n=== 变异 2：写死 CRLF ===");
fs.writeFileSync(P, orig, "utf8");
const m2 = orig.replace(
  'const scmeol = scmOrig.indexOf("\\r\\n") >= 0 ? "\\r\\n" : "\\n";',
  'const scmeol = "\\r\\n";'
);
const hasEol = m2 !== orig;
if (hasEol) {
  fs.writeFileSync(P, m2, "utf8");
  const r2 = run();
  console.log("  退出成功？ " + r2.ok + "   ← 必须 false");
  console.log(r2.out.split(/\r?\n/).filter((l) => /FAIL|修法/.test(l)).map((l) => "  " + l).join("\n"));
} else {
  console.log("  （当前源码没有 scmeol 那行，跳过 —— 改用别的形状）");
  const m3 = orig.replace('const ROOT = path.join(__dirname, "..");', 'const X = "D:\\\\tmp";');
  fs.writeFileSync(P, m3, "utf8");
  const r3 = run();
  console.log("  改用 D:\\tmp 形状，退出成功？ " + r3.ok + "   ← 必须 false");
  console.log(r3.out.split(/\r?\n/).filter((l) => /FAIL|修法/.test(l)).map((l) => "  " + l).join("\n"));
}

fs.writeFileSync(P, orig, "utf8");
const after = run();
console.log("\n还原后基线：" + (after.ok ? "OK（绿）" : "FAIL（仍红）"));
process.exit(!b.ok || r1.ok || !after.ok ? 1 : 0);
