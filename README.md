# supabased

![Architecture Diagram](.github/assets/diagram.png)

## GitHub OAuth Login

Supabased uses GitHub OAuth device flow for CLI login. Create a GitHub OAuth App for your organization, then enable Device Flow in the app settings.

Configure the server with the OAuth app client ID and the organization that is allowed to use Supabased:

```bash
export GITHUB_OAUTH_CLIENT_ID=Iv1.example
export GITHUB_ORG=your-org
```

The server requests the `read:org` scope so private organization membership checks can succeed. Developers authenticate with:

```bash
supabased login
```

The CLI prints a one-time GitHub code, opens the verification URL when possible, and stores only the Supabased session token after authorization completes.

## Tailscale TLS Deployment

For a private deployment, run the server on a Tailscale node and connect by its MagicDNS name:

```text
DNS: your-host.your-tailnet.ts.net
Tailscale IPv4: 100.x.y.z
```

Use the DNS name for clients so TLS hostname verification can match the certificate.

On the server, issue a Tailscale HTTPS certificate:

```bash
sudo tailscale cert your-host.your-tailnet.ts.net
```

Place the generated files somewhere readable by the server process. The `install`
commands below copy the files and set permissions in one step; moving the files
manually is also fine as long as `TLS_CERT` and `TLS_KEY` point to the final paths.

```bash
sudo mkdir -p /etc/supabased
sudo install -m 0644 your-host.your-tailnet.ts.net.crt /etc/supabased/your-host.your-tailnet.ts.net.crt
sudo install -m 0600 your-host.your-tailnet.ts.net.key /etc/supabased/your-host.your-tailnet.ts.net.key
```

The certificate can be world-readable, but the private key should be readable
only by the user that runs the server.

Start the server bound to the Tailscale IP with TLS enabled:

```bash
BIND_ADDR=100.x.y.z:50051 \
TLS_CERT=/etc/supabased/your-host.your-tailnet.ts.net.crt \
TLS_KEY=/etc/supabased/your-host.your-tailnet.ts.net.key \
cargo run -p supabased-server
```

Developers connect to the MagicDNS HTTPS endpoint:

```bash
supabased --server https://your-host.your-tailnet.ts.net:50051 login
```

The CLI defaults to local development at `http://[::1]:50051` until a server URL is saved by `supabased login` or supplied with `--server`. Plaintext binds are allowed only on loopback; any non-loopback `BIND_ADDR` requires `TLS_CERT` and `TLS_KEY`.
