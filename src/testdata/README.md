These fixtures contain generated sine waves, not recorded music.

Generated with FFmpeg 7.1.5 on 2026-11-15:

```
ffmpeg -f lavfi -i sine=frequency=440:sample_rate=44100:duration=6 -ac 2 -c:a aac -b:a 32k -movflags +frag_keyframe+empty_moov+default_base_moof -frag_duration 1000000 -f mp4 sine6-fragmented-aac.m4a
ffmpeg -f lavfi -i sine=frequency=440:sample_rate=48000:duration=6 -ac 2 -c:a libopus -b:a 32k -f webm sine6-opus.webm
```

SHA256:

- AAC: bbbaeddd57282c1ec1020eda436c48a5fa78ed0d6cbc351c35b6dfd43841fa0d
- Opus: 2284eb80977698ca593a774853b0d2234e08478c3baa57a968c9210e19da132a

The AAC regression test inserts a valid MP4 free box before the final fragment
and removes its optional, now-stale mfra index. It proves that resuming from the
available prefix does not depend on a missing later block.
