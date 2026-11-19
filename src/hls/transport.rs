//! Extract the ADTS audio track from a complete MPEG-TS HLS segment.

use bytes::Bytes;
use mpeg2ts_reader::StreamType;
use mpeg2ts_reader::demultiplex::{self, Demultiplex, FilterChangeset, FilterRequest, NullPacketFilter, PatPacketFilter, PmtPacketFilter};
use mpeg2ts_reader::packet::{Packet, Pid};
use mpeg2ts_reader::pes::{ElementaryStreamConsumer, PesContents, PesHeader, PesPacketFilter};

mpeg2ts_reader::packet_filter_switch! {
    Filter<AudioContext> {
        Pat: PatPacketFilter<AudioContext>,
        Pmt: PmtPacketFilter<AudioContext>,
        Audio: PesPacketFilter<AudioContext, AudioConsumer>,
        Ignore: NullPacketFilter<AudioContext>,
    }
}

#[derive(Default)]
pub struct AudioContext {
    changes: FilterChangeset<Filter>,
    audio_pid: Option<Pid>,
    audio: Vec<u8>,
    damaged: bool,
}

impl demultiplex::DemuxContext for AudioContext {
    type F = Filter;

    fn filter_changeset(&mut self) -> &mut FilterChangeset<Filter> {
        &mut self.changes
    }

    fn construct(&mut self, request: FilterRequest<'_, '_>) -> Filter {
        match request {
            FilterRequest::ByPid(Pid::PAT) => Filter::Pat(PatPacketFilter::default()),
            FilterRequest::Pmt { pid, program_number } => Filter::Pmt(PmtPacketFilter::new(pid, program_number)),
            FilterRequest::ByStream { stream_type: StreamType::ADTS, stream_info, .. }
                if *self.audio_pid.get_or_insert(stream_info.elementary_pid()) == stream_info.elementary_pid() =>
            {
                Filter::Audio(PesPacketFilter::new(AudioConsumer))
            }
            _ => Filter::Ignore(NullPacketFilter::default()),
        }
    }
}

pub struct AudioConsumer;

impl ElementaryStreamConsumer<AudioContext> for AudioConsumer {
    fn start_stream(&mut self, _: &mut AudioContext) {}

    fn begin_packet(&mut self, ctx: &mut AudioContext, header: PesHeader<'_>) {
        match header.contents() {
            PesContents::Parsed(Some(packet)) => ctx.audio.extend_from_slice(packet.payload()),
            _ => ctx.damaged = true,
        }
    }

    fn continue_packet(&mut self, ctx: &mut AudioContext, data: &[u8]) {
        ctx.audio.extend_from_slice(data);
    }

    fn end_packet(&mut self, _: &mut AudioContext) {}

    fn continuity_error(&mut self, ctx: &mut AudioContext) {
        ctx.damaged = true;
    }
}

pub(crate) fn unpack_audio(segment: &[u8]) -> anyhow::Result<Bytes> {
    let (packets, remainder) = segment.as_chunks::<{ Packet::SIZE }>();
    anyhow::ensure!(!packets.is_empty() && remainder.is_empty()
        && packets.iter().all(|packet| packet[0] == 0x47), "invalid MPEG-TS segment");
    let mut ctx = AudioContext::default();
    Demultiplex::new(&mut ctx).push(&mut ctx, segment);
    anyhow::ensure!(!ctx.damaged, "damaged MPEG-TS audio track");
    anyhow::ensure!(!ctx.audio.is_empty(), "no AAC frames in MPEG-TS segment");
    let mut offset = 0;
    while offset < ctx.audio.len() {
        let frame = super::adts_at(&ctx.audio, offset)
            .ok_or_else(|| anyhow::anyhow!("incomplete or invalid AAC frame in MPEG-TS segment"))?;
        offset = frame.next;
    }
    Ok(ctx.audio.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_transport_packet_cannot_leave_a_partial_aac_frame() {
        // A 440 Hz sine at 44.1 kHz, encoded as stereo AAC in MPEG-TS.
        let segment = include_bytes!("testdata/aac.ts");
        let audio = unpack_audio(segment).unwrap();
        assert_eq!(audio.len(), 1379);

        // The last AAC frame spans two TS packets. Dropping the last packet leaves
        // a valid TS sync and continuity count, but fewer bytes than ADTS declares.
        assert!(unpack_audio(&segment[..segment.len() - Packet::SIZE]).is_err());
    }
}
