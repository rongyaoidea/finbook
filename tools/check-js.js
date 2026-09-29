// JS 语法检查：按**退出码**判定，不靠肉眼看输出。
//
// 为什么要这个脚本：`node --check file.js 2>&1 | Select-Object -First 1` 在
// PowerShell 里会把「有错」和「没错」都显示成一段 stderr 文本，肉眼判断极容易
// 看错——我已经被骗过一次：`node --check` 报了语法错（app.js:1061 缺一个 `}`），
// 而我 `Select-Object -First 1` 只截到 PowerShell 的错误头，紧接着的
// `echo "js ok 若空"` 照样打印，于是我以为检查通过了，把一个**整个前端都跑不起来**
// 的文件当成好的往下做。
//
// 正确做法：看 $LASTEXITCODE，或用本脚本直接汇总「哪些文件不通过」。
const { execFileSync } = require("child_process");
const fs = require("fs");
const path = require("path");

const targets = [];
const walk = (dir) => {
  for (const e of fs.readdirSync(dir, { withFileTypes: true })) {
    const p = path.join(dir, e.name);
    if (e.isDirectory()) {
      if (e.name !== "node_modules" && e.name !== "test-results") walk(p);
    } else if (e.name.endsWith(".js")) {
      targets.push(p);
    }
  }
};
walk(path.join(__dirname, "..", "crates", "finweb", "static"));
walk(path.join(__dirname, "..", "e2e", "tests"));
const helpers = path.join(__dirname, "..", "e2e", "helpers.js");
if (fs.existsSync(helpers)) targets.push(helpers);

let bad = 0;
for (const f of targets) {
  try {
    execFileSync(process.execPath, ["--check", f], { stdio: "pipe" });
  } catch (e) {
    bad++;
    console.log("FAIL " + path.relative(path.join(__dirname, ".."), f));
    const out = ((e.stderr || Buffer.alloc(0)).toString() + (e.stdout || Buffer.alloc(0)).toString())
      .split("\n")
      .slice(0, 4)
      .join("\n    ");
    console.log("    " + out);
  }
}

// ---------------------------------------------------------------------------
// 重复 id 检查（只在**同一个函数内**比对）
//
// 为什么要单独查：**重复 id 是完全合法的 JavaScript，语法检查抓不到**，但后果
// 和语法错一样致命——`$("#ak-list")` 只操作第一个，第二个永远停在「加载中…」，
// 页面上看起来「有个列表但没数据」，E2E 报 strict mode violation，排查方向
// 还会被带偏到「接口没返回」。
//
// 我为此白跑了一轮 9.7 分钟的 E2E：把 app.js 回退到 HEAD 后重放自己的编辑，
// 而 HEAD 里本来就有密钥管理块，我又加了一份，页面上出现两个 `#ak-list`。
//
// **只在同一函数内比**，因为全文件比对会误报 23 处：同一个 id 在不同视图各写
// 一份是完全正常的（同一时刻只渲染一个视图）。一个会误报 23 次的检查等于没有
// 检查——人会开始无视它，那真 bug 就一起漏了。函数边界用「顶格的 function /
// async function」近似，本文件的视图函数都是这个写法。
const STATIC = path.join(__dirname, "..", "crates", "finweb", "static");
const idTargets = [path.join(STATIC, "app.js"), path.join(STATIC, "index.html")].filter((f) =>
  fs.existsSync(f)
);
const TOP_FN = /^(?:async\s+)?function\s+\w+|^(?:const|let|var)\s+\w+\s*=\s*(?:async\s*)?\(/;
for (const f of idTargets) {
  const src = fs.readFileSync(f, "utf8");
  const lines = src.split(/\r?\n/);
  // 顶层函数名 -> Map(id -> 行号)
  const scopes = new Map();
  let scope = "<module>";
  lines.forEach((line, i) => {
    if (TOP_FN.test(line)) {
      const m = /(\w+)\s*[=(]/.exec(line);
      scope = m ? m[1] : "<anon>";
    }
    if (!scopes.has(scope)) scopes.set(scope, new Map());
    const inScope = scopes.get(scope);
    const re = /\bid="([A-Za-z][\w:.-]*)"/g;
    let m;
    while ((m = re.exec(line)) !== null) {
      const id = m[1];
      if (!inScope.has(id)) inScope.set(id, []);
      inScope.get(id).push(i + 1);
    }
  });
  for (const [fn, ids] of scopes) {
    for (const [id, at] of ids) {
      // 同一行内的重复是**故意的互斥分支**，不是 bug。
      // 典型写法：`${list.length ? `<select id="x">…` : `<input id="x" …>`}`
      // 两者只渲染一个，而读值都走 `$("#x").value`，对 select 和 input 都成立。
      // 把它判成错就等于逼着人把 id 拆成两个，只会让代码变丑而不会变对。
      if (at.length > 1 && new Set(at).size === 1) continue;
      if (at.length > 1) {
        bad++;
        console.log(
          `FAIL 重复 id #${id} 在 ${path.relative(path.join(__dirname, ".."), f)} 的 ` +
            `${fn}() 内出现 ${at.length} 次（第 ${at.join("、")} 行）——` +
            `$("#${id}") 只会操作第一个，另一个永远不会被更新`
        );
      }
    }
  }
}

// ---------------------------------------------------------------------------
// nginx sub_filter 锚点检查
//
// 为什么要查这个：某些部署的反代并不只是"转发"，它还在**下发时改写 app.js**
// （在 vhost 里配 sub_filter，具体配置路径因面板而异，不写进本仓库）：
//
//     sub_filter 'r.status === 401'
//                'r.status === 401 || (r.status === 403 && data && data.error === "未登录或会话已失效")';
//     header_filter_by_lua_block { if ngx.status == 401 and upstream_addr ~= "" then ngx.status = 403 end }
//
// 起因：finweb 的 401 不带 WWW-Authenticate，浏览器收到后会把已缓存的 Basic
// 凭据一起清掉，下一轮又撞上第一道 basic 框 —— 用户表现为"登录两遍"。
// 于是反代把上游 401 改写成 403，并让前端把"403 + 未登录"也当会话失效处理。
//
// **这个补丁靠 app.js 里的字面量 `r.status === 401` 生效。** 一旦重构
// api()（换变量名、改成 switch、抽成常量）字面量消失，sub_filter 就静默变成
// 空操作，症状是"用户偶发要登录两遍"——本地和 CI 全绿，只有生产复现。
// 语法检查抓不到这种"部署配置依赖源码字面量"的耦合，所以在这里机器化。
//
// 判定（两条都要满足）：
//   1. 该字面量在 app.js 里**恰好出现 1 次**——0 次 = 补丁失效；
//      >1 次 = 补丁会在多处注入，可能改到不该改的分支。
//   2. 它前面（同一函数内）必须有 `let data = null` 之类的 data 声明，
//      否则注入的 `data &&` 引用未定义变量 → 直接 ReferenceError，整个前端白屏。
// ---------------------------------------------------------------------------
const ANCHOR = "r.status === 401";
const appFile = path.join(STATIC, "app.js");
if (fs.existsSync(appFile)) {
  const src = fs.readFileSync(appFile, "utf8");
  const hits = src.split(ANCHOR).length - 1;
  if (hits !== 1) {
    bad++;
    console.log(
      `FAIL nginx sub_filter 锚点 \`${ANCHOR}\` 在 app.js 里出现 ${hits} 次（应为 1 次）——\n` +
        `    生产 nginx 靠这个字面量把上游 401 改写成 403 并让前端识别会话失效；\n` +
        `    0 次 = 补丁静默失效，症状是「用户要登录两遍」；>1 次 = 补丁会改到多处分支。\n` +
        `    修法：保证 api() 里恰有一处 \`if (r.status === 401) {\`，不要抽成常量或换写法。`
    );
  } else {
    // 注入的表达式里用了 data，必须在同一函数作用域内已声明
    const at = src.indexOf(ANCHOR);
    // 往上找最近的函数起点
    const before = src.slice(0, at);
    const fnStart = Math.max(
      before.lastIndexOf("\nasync function "),
      before.lastIndexOf("\nfunction ")
    );
    const scope = src.slice(fnStart < 0 ? 0 : fnStart, at);
    if (!/\b(let|const|var)\s+data\b/.test(scope)) {
      bad++;
      console.log(
        `FAIL nginx sub_filter 锚点 \`${ANCHOR}\` 所在函数里没有 data 声明——\n` +
          `    nginx 注入的 \`data && data.error === "未登录或会话已失效"\` 会引用未定义的 data，\n` +
          `    结果是整页 ReferenceError 白屏。修法：在 401 判断之前声明 data。`
      );
    }
  }
}

console.log(
  bad === 0
    ? `OK: ${targets.length} 个 JS 文件语法通过 + ${idTargets.length} 个文件无重复 id + nginx sub_filter 锚点在位`
    : `FAIL: ${bad} 个问题`
);
process.exit(bad === 0 ? 0 : 1);
