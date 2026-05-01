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
