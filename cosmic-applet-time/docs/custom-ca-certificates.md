# Custom CA Certificate Support

When connecting to self-hosted CalDAV or ICS URL servers that use a private
Certificate Authority (CA), the applet will reject the TLS connection by
default because the server's certificate is not signed by a publicly trusted
CA.

The **CA Certificate** field in the source configuration allows you to provide
the path to your CA's root certificate so the applet trusts your server.

## When is this needed?

- Self-hosted CalDAV servers (Radicale, Baikal, Nextcloud, etc.) using TLS
  certificates signed by a private or internal CA.
- Corporate environments where an internal CA issues server certificates.
- Any server whose certificate chain is not in the system trust store.

## Expected file format

The certificate file must be **PEM-encoded** (Base64 ASCII). It typically
starts with `-----BEGIN CERTIFICATE-----` and ends with
`-----END CERTIFICATE-----`.

If your CA has intermediate certificates, concatenate them into a single PEM
file with the root CA last.

## How to use

1. Export your CA's root certificate in PEM format. For example, with
   `step-ca`:

   ```sh
   step ca root root_ca.crt
   ```

   Or extract from a PKCS#12 bundle:

   ```sh
   openssl pkcs12 -in bundle.p12 -cacerts -nokeys -out root_ca.pem
   ```

2. Place the PEM file somewhere accessible, e.g.
   `~/.config/cosmic/ca-certs/myca.pem`.

3. In the applet, add or edit a calendar source (CalDAV or ICS URL).

4. In the **CA Certificate (PEM path)** field, enter the absolute path to the
   PEM file, e.g. `/home/user/.config/cosmic/ca-certs/myca.pem`.

5. Save the source. The applet will load the certificate and add it to the
   TLS trust store for requests to that source.

## Notes

- Each source has its own CA certificate setting. Different sources can use
  different CAs.
- If the path is left empty, only the system's default trust store is used.
- The certificate is loaded at sync time. If you update the PEM file, changes
  take effect on the next sync.
- ICS File sources do not use network requests, so the CA certificate field
  does not apply to them.
