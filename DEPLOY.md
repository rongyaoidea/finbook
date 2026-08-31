# FinBook 生产部署指南

> 本文面向「把 FinBook Web 端（finweb）正式部署到服务器」的场景。
> 桌面端（finbook）无需部署——直接分发可执行文件给会计人员即可。

---

## 1. 架构总览

```
浏览器 ──HTTPS──> 反向代理 (nginx / caddy) ──HTTP──> finweb :8080
                                                        │
                                                        └── SQLite 账套 (.fbk, WAL 模式)
```

- **finweb**：Axum HTTP 服务，监听 `0.0.0.0:8080`（可用环境变量改）
- **账套**：一个 `.fbk` 文件 = 标准 SQLite 库，服务端本地磁盘存放
- **多用户**：同一服务器上多浏览器同时登录同一账套，由 WAL + `busy_timeout=5s` 保证并发安全
- **会话**：登录态存**服务进程内存**（见 §5 限制），部署时务必保持单进程

## 2. 环境变量

| 变量 | 默认值 | 说明 |
|---|---|---|
| `FINBOOK_DB` | `./finbook.fbk` | 账套文件路径；不存在则自动新建（空账套） |
| `FINBOOK_LISTEN` | `0.0.0.0:8080` | 监听地址（生产建议 `127.0.0.1:8080` + 反向代理） |
| `FINWEB_STATIC_DIR` | 可执行文件同级 `static/` | 前端静态资源目录 |

> **首次登录即管理员**：新账套没有任何用户时，第一个成功登录的账号会自动成为系统管理员。
> 登录页会提示「管理员账号」是否已设定。

## 3. 方式一：systemd 直接部署（推荐 Linux 服务器）

1. 编译：`cargo build --release -p finweb`
2. 复制产物与静态资源：
   ```bash
   sudo mkdir -p /opt/finbook
   sudo cp target/release/finweb /opt/finbook/
   sudo cp -r crates/finweb/static /opt/finbook/
   ```
3. 创建专用用户、数据目录并安装服务（详见 `deploy/finweb.service` 注释）：
   ```bash
   sudo useradd --system --home /opt/finbook --shell /usr/sbin/nologin finbook
   sudo mkdir -p /opt/finbook/data && sudo chown -R finbook:finbook /opt/finbook
   sudo cp deploy/finweb.service /etc/systemd/system/
   sudo systemctl daemon-reload && sudo systemctl enable --now finweb
   ```
4. 验证：`curl http://127.0.0.1:8080/api/health` 应返回 `ok`

## 4. 方式二：Docker 部署

```bash
docker build -t finbook:latest .
docker run -d --name finbook -p 127.0.0.1:8080:8080 \
  -v finbook_data:/data \
  -e FINBOOK_LISTEN=0.0.0.0:8080 \
  finbook:latest
```

- 账套文件在卷 `finbook_data` 下的 `/data/finbook.fbk`
- 镜像内置 `HEALTHCHECK`（探测 8080 端口）
- 数据卷务必定期备份（见 §6），容器销毁不影响账套（卷独立）

## 5. HTTPS 反向代理（必须）

finweb 本身只提供 HTTP。**生产环境禁止把 8080 裸暴露到公网**（口令明文传输）。
推荐用 nginx 或 caddy 终结 TLS，把请求转发到 `127.0.0.1:8080`。

### nginx 配置示例

```nginx
server {
    listen 443 ssl;
    server_name finance.example.com;
    ssl_certificate     /etc/letsencrypt/live/finance.example.com/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/finance.example.com/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8080;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
        # 会话 cookie 仅随导航请求携带，SSE/长连接场景不需要关缓冲
        proxy_buffering off;
    }
}

server {
    listen 80;
    server_name finance.example.com;
    return 301 https://$host$request_uri;
}
```

### caddy 配置示例（自动 HTTPS）

```
finance.example.com {
    reverse_proxy 127.0.0.1:8080
}
```

> 证书建议用 Let's Encrypt（certbot 或 caddy 自动签发）。cookie 当前为
> `HttpOnly; SameSite=Lax`，在纯 HTTPS 下会话是安全的；部署时请确保浏览器
> 访问地址始终是 `https://`。

## 6. 备份策略（必须）

账套是一个文件，备份 = 复制文件：

- **热备**：finweb 运行时可以直接执行
  ```bash
  sqlite3 /opt/finbook/data/finbook.fbk "VACUUM INTO '/backup/finbook-$(date +%F).fbk'"
  ```
  或三级备份脚本。`VACUUM INTO` 产出紧凑副本，不影响运行。
- **冷备**：`systemctl stop finweb` 后直接 `cp` 账套文件。
- **轮转建议**：每日全量 + 保留 30 天：
  ```bash
  # /etc/cron.daily/finbook-backup
  #!/bin/sh
  BK=/backup/finbook
  mkdir -p $BK
  sqlite3 /opt/finbook/data/finbook.fbk \
    "VACUUM INTO '$BK/finbook-$(date +\%F).fbk'" 2>/dev/null \
  && find $BK -name 'finbook-*.fbk' -mtime +30 -delete
  ```
- **外置附件**：大于 256KB 的附件落在账套同目录 `.attachments/`，备份时**必须连同该目录一起打包**。

> ⚠️ 不要把 `.fbk` 放在 NFS/SMB 网络盘被多台电脑并发直开（WAL 在网络文件系统上不稳定）。
> 多用户请统一走「服务器本机运行 + 浏览器访问」模式。

## 7. 会话与多实例限制（重要）

- 登录会话存 **finweb 进程内存**：进程重启后所有用户需重新登录（可接受，账套数据不受影响）。
- **不要**在同一账套上水平扩容为多个 finweb 实例：会话不共享且无分布式锁，
  SQLite 并发写虽安全但多实例会放大锁竞争。单服务器单进程即可支撑中小团队。
- 若确需高可用，方案是「主库 + 定期 VACUUM INTO 副本」用备份恢复，而不是多活。

## 8. 运维命令

```bash
# 健康检查
curl http://127.0.0.1:8080/api/health

# 查看日志
journalctl -u finweb -f

# 升级流程（替换二进制即可，schema 自动迁移）
sudo systemctl stop finweb
sudo cp target/release/finweb /opt/finbook/
sudo systemctl start finweb

# 一键重置所有账号口令为 admin123（交接/忘密）
cargo run -p findb --release --example reset_pwd -- /opt/finbook/data/finbook.fbk
```

## 9. 安全检查清单

- [ ] 反向代理已启用 HTTPS，8080 未对外暴露
- [ ] systemd 服务开了加固参数（`deploy/finweb.service` 已包含）
- [ ] 账套目录权限 `finbook:finbook` 私有
- [ ] 每日备份 cron 已生效且有轮转
- [ ] 管理员账号已改默认口令，普通账号按角色开通
- [ ] 首次登录后确认「管理员账号」提示消失