// Frontend runtime config.
//
// FAUCET_API_BASE is the origin of the faucet API. Leave empty for same-origin
// (the bundled UI served by the faucet binary). When the frontend is hosted
// separately (e.g. AWS Amplify), the Amplify build overwrites this file from the
// FAUCET_API_BASE environment variable (see amplify.yml) to point at the API,
// e.g. "https://<elastic-ip>.nip.io".
window.FAUCET_API_BASE = "";
