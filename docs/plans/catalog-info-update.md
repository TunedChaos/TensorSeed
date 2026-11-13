# Pending: official catalog contact and policy URL

The official `tensorseed` catalog was created with an empty
`publisher.contact` and `publisher.policy_url`, and with empty
`webseed_bases` and `trackers`. SPEC section 11 (takedown process) expects a
published contact and policy URL.

Prepared, not published. At the next catalog publish, on the publisher
machine, set in the publisher config:

```toml
[catalog]
publisher_name = "Tuned Chaos"
publisher_contact = "https://github.com/TunedChaos/TensorSeed/issues/new?template=takedown.yml"
publisher_policy_url = "https://github.com/TunedChaos/TensorSeed/blob/main/SECURITY.md#requesting-a-catalog-takedown"
```

Then:

```sh
tensorseed catalog update-info --dry-run   # review current vs proposed
tensorseed catalog update-info             # signs and publishes one revision
tensorseed catalog anchor                  # if the publish did not anchor it
```

Check the proposed `webseed_bases` in the dry run. With the mirror bucket
configured as a storage target it should be
`["https://mirror.tensorseed.dev/t/"]`. A dry run on a build machine with
only that target produced:

```json
{"created_at":"2026-11-12T22:22:04Z","name":"tensorseed","publisher":{"contact":"https://github.com/TunedChaos/TensorSeed/issues/new?template=takedown.yml","display_name":"Tuned Chaos","policy_url":"https://github.com/TunedChaos/TensorSeed/blob/main/SECURITY.md#requesting-a-catalog-takedown"},"schema":"ts.catalog/1","trackers":[],"webseed_bases":["https://mirror.tensorseed.dev/t/"]}
```
