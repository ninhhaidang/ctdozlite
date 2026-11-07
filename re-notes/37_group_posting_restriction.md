# Group posting restriction

The bundled Zalo web client maps `ONLY_ADMIN_CHAT` to `lockSendMsg` in
`web-bundle/default-embed-render.js`. The published group-info response shape
places `lockSendMsg` under `setting` alongside `creatorId` and `adminIds`:
https://docs.salework.net/salework-tai-lieu/salework-zalo-open-api/contact/thong-tin-nhom/lay-thong-tin-nhom

The client uses the existing group-info refresh to hide the composer for ordinary
members when the flag is set. Missing role or setting data leaves the permission
unknown. Live verification still requires a restricted group account, an owner or
deputy account, and a refresh after toggling the group setting.
