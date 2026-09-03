//! RTMP streaming start/stop. Mirrors the FFmpeg pipeline in `record.rs`,
//! replacing the file muxer with an FLV push to the configured RTMP ingest
//! (YouTube by default, see Settings → Streaming).
//!
//! Frames arrive the same way they do for recording: the UI tick composes one
//! output-resolution BGRA frame (`capture.rs`) and sends it over an mpsc
//! channel that a dedicated writer thread drains into FFmpeg's stdin.

use super::state::EventLogKind;
use super::RobsController;
use std::process::Stdio;

/// Join an RTMP server URL and stream key into the full push URL.
///
/// Pure string helper so it can be unit-tested without touching the network.
/// Trims whitespace from both parts and collapses any trailing `/` on the
/// server so `rtmp://host/app/` + `key` does not become `.../app//key`.
fn build_rtmp_url(server: &str, key: &str) -> Result<String, String> {
    let server = server.trim();
    let key = key.trim();
    if server.is_empty() {
        return Err("stream server is not set".into());
    }
    if key.is_empty() {
        return Err("stream key is not set".into());
    }
    Ok(format!("{}/{}", server.trim_end_matches('/'), key))
}

impl RobsController {
    pub fn start_streaming(&mut self) {
        if self.streaming {
            return;
        }

        let url = match build_rtmp_url(&self.stream_server, &self.stream_key) {
            Ok(url) => url,
            Err(reason) => {
                self.log_event(
                    format!("Stream not started: {reason} (Settings \u{2192} Streaming)"),
                    EventLogKind::Stream,
                );
                return;
            }
        };

        let encoder =
            if self.video_encoder.contains("NVENC") || self.video_encoder.contains("NVIDIA") {
                "h264_nvenc"
            } else {
                "libx264"
            };

        let output_w = self.output_width;
        let output_h = self.output_height;

        let mut args: Vec<String> = Vec::new();

        // Input: raw BGRA frames at output resolution, same as the recording
        // pipeline (frames are already scaled when they reach us).
        args.extend(["-f", "rawvideo", "-pix_fmt", "bgra"].map(String::from));
        args.push("-video_size".into());
        args.push(format!("{}x{}", output_w, output_h));
        args.push("-framerate".into());
        args.push(self.fps_setting.to_string());
        args.push("-i".into());
        args.push("pipe:0".into());

        // Video codec + FLV-compatible pixel format (YouTube/Twitch ingest
        // expects yuv420p; BGRA input would be rejected or misrendered).
        args.push("-c:v".into());
        args.push(encoder.to_string());
        args.push("-pix_fmt".into());
        args.push("yuv420p".into());

        // Streaming-tuned rate control: CBR is what live ingest wants.
        let bitrate = self.stream_bitrate;
        let gop = ((self.fps_setting * self.keyframe_interval as f32) as u32).max(1);
        if encoder == "h264_nvenc" {
            args.extend(["-preset", "p4", "-tune", "ll", "-gpu", "0", "-rc", "cbr"].map(String::from));
        } else {
            args.extend(["-preset", "veryfast", "-tune", "zerolatency"].map(String::from));
        }
        args.push("-b:v".into());
        args.push(format!("{}k", bitrate));
        args.push("-maxrate".into());
        args.push(format!("{}k", bitrate));
        args.push("-bufsize".into());
        args.push(format!("{}k", bitrate * 2));
        args.push("-g".into());
        args.push(gop.to_string());
        args.push("-r".into());
        args.push(self.fps_setting.to_string());

        // Output: FLV over RTMP.
        args.push("-f".into());
        args.push("flv".into());
        args.push(url.clone());

        eprintln!("[Stream] FFmpeg args: {:?}", args);

        let spawn = std::process::Command::new("ffmpeg")
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = match spawn {
            Ok(child) => child,
            Err(e) => {
                self.log_event(format!("Stream failed to start: {e}"), EventLogKind::Stream);
                return;
            }
        };

        let pid = child.id();
        eprintln!("[Stream] FFmpeg started (PID: {pid}) pushing to {url}");

        // Drain stderr on a background thread: RTMP connect/auth failures are
        // reported there, and an undrained pipe would back-pressure FFmpeg on
        // a long stream. Mirror to stderr so failures are visible in console.
        if let Some(stderr) = child.stderr.take() {
            std::thread::spawn(move || {
                use std::io::{BufRead, BufReader};
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    eprintln!("[Stream:ffmpeg] {line}");
                }
            });
        }

        // Writer thread: drain the frame channel into FFmpeg stdin (identical
        // to the recording writer; stdin EOF on exit lets FFmpeg finalize).
        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let stop_flag = self.stream.stop_flag.clone();
        let stdin = child
            .stdin
            .take()
            .expect("FFmpeg stdin should be piped");
        let writer = std::thread::spawn(move || {
            let mut stdin = stdin;
            let mut frame_count: u64 = 0;
            while !stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
                match rx.recv_timeout(std::time::Duration::from_millis(100)) {
                    Ok(frame) => {
                        if std::io::Write::write_all(&mut stdin, &frame).is_err() {
                            eprintln!("[Stream] Failed to write frame to FFmpeg");
                            break;
                        }
                        frame_count += 1;
                        if frame_count.is_multiple_of(300) {
                            eprintln!("[Stream] Written {frame_count} frames to FFmpeg");
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                }
            }
            eprintln!("[Stream] Writer thread exiting after {frame_count} frames");
        });

        self.stream.ffmpeg_handle = Some(child);
        self.stream.writer_thread = Some(writer);
        self.stream.frame_sender = Some(tx);
        self.stream.frame_count = 0;

        self.streaming = true;
        self.streaming_paused = false;
        self.streaming_time = 0;
        self.bitrate = self.stream_bitrate;

        let host = self.stream_server.trim_start_matches("rtmp://").to_string();
        self.log_event(
            format!("Streaming started \u{2192} {} ({})", host, self.stream_service),
            EventLogKind::Stream,
        );
    }

    pub fn stop_streaming(&mut self) {
        eprintln!("[Stream] Stopping stream...");

        // 1. Signal the writer thread, 2. close the channel, 3. join the
        // writer (dropping FFmpeg stdin = EOF), 4. reap the child.
        self.stream
            .stop_flag
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.stream.frame_sender = None;
        if let Some(handle) = self.stream.writer_thread.take() {
            let _ = handle.join();
        }

        if let Some(mut child) = self.stream.ffmpeg_handle.take() {
            let timeout = std::time::Duration::from_secs(10);
            let start = std::time::Instant::now();
            loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        eprintln!("[Stream] FFmpeg exited with: {status}");
                        break;
                    }
                    Ok(None) => {
                        if start.elapsed() > timeout {
                            eprintln!("[Stream] FFmpeg timeout, killing this process only");
                            // Never `taskkill /IM ffmpeg.exe` here — it would
                            // also kill a concurrent recording's FFmpeg.
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(200));
                    }
                    Err(e) => {
                        eprintln!("[Stream] Error waiting for FFmpeg: {e}");
                        break;
                    }
                }
            }
        }

        // Reset for the next session.
        self.stream.stop_flag =
            std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.stream.writer_thread = None;
        self.stream.frame_sender = None;
        self.stream.ffmpeg_handle = None;

        let was_streaming = self.streaming;
        let elapsed = self.streaming_time / 1000; // ms → s
        self.streaming = false;
        self.streaming_paused = false;
        self.streaming_time = 0;

        if was_streaming {
            let duration = Self::format_time(elapsed);
            self.log_event(
                format!("Streaming stopped ({duration})"),
                EventLogKind::Stream,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::build_rtmp_url;

    #[test]
    fn joins_server_and_key() {
        assert_eq!(
            build_rtmp_url("rtmp://a.rtmp.youtube.com/live2", "abc123-xyz").unwrap(),
            "rtmp://a.rtmp.youtube.com/live2/abc123-xyz"
        );
    }

    #[test]
    fn trims_whitespace_and_trailing_slash() {
        assert_eq!(
            build_rtmp_url("  rtmp://live.twitch.tv/app/ ", " k3y \n").unwrap(),
            "rtmp://live.twitch.tv/app/k3y"
        );
    }

    #[test]
    fn empty_server_is_an_error() {
        assert!(build_rtmp_url("", "key").is_err());
        assert!(build_rtmp_url("   ", "key").is_err());
    }

    #[test]
    fn empty_key_is_an_error() {
        assert!(build_rtmp_url("rtmp://host/app", "").is_err());
        assert!(build_rtmp_url("rtmp://host/app", "  ").is_err());
    }

    #[test]
    fn does_not_touch_key_slashes() {
        // A key containing a slash is the user's problem; we only normalize
        // the server side and must not mangle the key itself.
        assert_eq!(
            build_rtmp_url("rtmp://host/app", "a/b").unwrap(),
            "rtmp://host/app/a/b"
        );
    }
}
