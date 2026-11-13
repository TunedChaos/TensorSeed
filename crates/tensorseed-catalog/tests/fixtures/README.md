# Test fixtures

Everything here is test data for `tensorseed-catalog`.

- `minisign-*.key` and `minisign-*.pub` are throwaway minisign key pairs
  generated only for the unit tests in `src/minisign.rs` (key ids
  `00D7B6C6CB72DB9E` and `C578D71A00D9B625`; the encrypted one uses the
  password written in the test). They have never signed anything outside
  these tests and are not the Tensorseed publisher key
  (`CE1E0E3D543DFFB8`).
- `minisign-test.*` is a signature test vector made with minisign 0.12.
- `testnet11-proof.json` is a DataLayer `get_proof` response captured from a
  public Chia testnet11 store.

Line endings and bytes must stay exactly as committed (see
`.gitattributes`), because signatures cover the raw bytes.
