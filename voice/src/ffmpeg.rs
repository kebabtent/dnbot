use common::discord::voice::pcm::{frame_sample_size, PcmCodec, PcmFrame, PcmStream};
use common::discord::voice::{EncodeError, OpusStream, SAMPLE_RATE};
use futures::Stream;
use pin_project::pin_project;
use std::pin::Pin;
use std::process::Stdio;
use std::task::{Context, Poll};
use tokio::process::{ChildStdout, Command};
use tokio_util::codec::FramedRead;

#[pin_project]
pub struct FfmpegStream {
	stereo: bool,
	#[pin]
	pipe: FramedRead<ChildStdout, PcmCodec>,
}

impl FfmpegStream {
	fn new(url: &str, stereo: bool) -> Result<Self, EncodeError> {
		let mut cmd = Command::new("ffmpeg")
			.args(&["-i", url, "-f", "s16le", "-ac", "2", "-ar"])
			.arg(format!("{}", SAMPLE_RATE))
			.args(&["-acodec", "pcm_s16le", "-"])
			.stdin(Stdio::null())
			.stdout(Stdio::piped())
			.stderr(Stdio::null())
			.spawn()?;
		let inner = cmd.stdout.take().unwrap();
		let pipe = FramedRead::new(inner, PcmCodec::new(frame_sample_size(stereo)));
		Ok(Self { stereo, pipe })
	}
}

impl Stream for FfmpegStream {
	type Item = Result<PcmFrame, EncodeError>;

	fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
		self.project().pipe.poll_next(cx).map_err(|e| e.into())
	}
}

impl PcmStream for FfmpegStream {
	fn is_stereo(&self) -> bool {
		self.stereo
	}
}

pub fn ffmpeg_stream(url: &str, stereo: bool, bitrate: u32) -> Result<OpusStream, EncodeError> {
	let stream = FfmpegStream::new(url, stereo)?;
	Ok(OpusStream::new(stream, bitrate)?)
}
