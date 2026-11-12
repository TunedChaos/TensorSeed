# M0 spikes

Throwaway programs behind [docs/M0-TORRENT-SPIKE.md](../docs/M0-TORRENT-SPIKE.md).
They are not workspace members and are not built by CI.

| Path | What it does |
|---|---|
| `rqbit/` | Rust program on librqbit 9.1.0: creates, seeds in place, and downloads torrents between two sessions on loopback; tries v2 only, hybrid, webseed only, and `btmh` magnets. `serve` and `get` modes for interop with libtorrent. |
| `libtorrent/make_torrents.py` | Builds reference v1, v2, and hybrid torrents with libtorrent 2.x. |
| `libtorrent/swarm.py` | The same capability checks with libtorrent 2.x (seed in place, peer download, webseed only, `btmh` magnet, download from an external seeder). |
| `libtorrent/crosscheck.py` | Compares info hashes from Tensorseed's builder (`cargo run --example mktorrent -p tensorseed-torrent`) with libtorrent's. |
| `webseed_server.py` | Static HTTP server with Range support, logging each request, for BEP 19 tests. |

Rerun (Debian or Ubuntu; needs `python3-libtorrent`):

```sh
D=/tmp/ts-spike; N=org__tiny@0123456789ab
mkdir -p $D/data/$N/sub $D/www/t $D/torrents
head -c 209715200 /dev/urandom > $D/data/$N/model-00001.safetensors
head -c 67108864  /dev/urandom > $D/data/$N/model-00002.safetensors
head -c 1000      /dev/urandom > $D/data/$N/config.json
head -c 40000     /dev/urandom > $D/data/$N/sub/tokenizer.json
ln -s $D/data/$N $D/www/t/$N
python3 spikes/webseed_server.py $D/www 18080 2> $D/http.log &
python3 spikes/libtorrent/make_torrents.py $D/data $N $D/torrents http://127.0.0.1:18080/t/ > $D/torrents/hashes.txt
(cd spikes/rqbit && cargo run --release -- $D/data $N $D/torrents 18080) | grep RESULT
python3 spikes/libtorrent/swarm.py $D/data $D/torrents
cargo build --release -p tensorseed-torrent --no-default-features --example mktorrent
python3 spikes/libtorrent/crosscheck.py target/release/examples/mktorrent $D/data/$N 4194304 16777216
```
