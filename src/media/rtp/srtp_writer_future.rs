use crate::error::{Error, Result};
use crate::media::dtls_transport::RTCDtlsTransport;
use crate::media::rtp::rtp_sender::RTPSenderInternal;
use crate::media::rtp::SSRC;

use srtp::session::Session;
use srtp::stream::Stream;

use async_trait::async_trait;
use bytes::Bytes;
use interceptor::{Attributes, RTCPReader, RTPWriter};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::Mutex;

/// SrtpWriterFuture blocks Read/Write calls until
/// the SRTP Session is available
pub(crate) struct SrtpWriterFuture {
    pub(crate) ssrc: SSRC,
    pub(crate) rtp_sender: Arc<RTPSenderInternal>,
    pub(crate) rtp_transport: Arc<RTCDtlsTransport>,
    pub(crate) rtcp_read_stream: Mutex<Option<Arc<Stream>>>, // atomic.Value // *
    pub(crate) rtp_write_session: Mutex<Option<Arc<Session>>>, // atomic.Value // *
}

impl SrtpWriterFuture {
    async fn init(&self, return_when_no_srtp: bool) -> Result<()> {
        if return_when_no_srtp {
            {
                if self.rtp_sender.stop_called_signal.load(Ordering::SeqCst) {
                    return Err(Error::ErrClosedPipe);
                }
            }

            if !self.rtp_transport.srtp_ready_signal.load(Ordering::SeqCst) {
                return Ok(());
            }
        } else {
            let mut rx = self.rtp_transport.srtp_ready_rx.lock().await;
            if let Some(srtp_ready_rx) = &mut *rx {
                let mut stop_called_rx = self.rtp_sender.stop_called_rx.lock().await;

                tokio::select! {
                    _ = stop_called_rx.recv()=> return Err(Error::ErrClosedPipe),
                    _ = srtp_ready_rx.recv() =>{}
                }
            }
        }

        if let Some(srtcp_session) = self.rtp_transport.get_srtcp_session().await {
            let rtcp_read_stream = srtcp_session.open(self.ssrc).await;
            let mut stream = self.rtcp_read_stream.lock().await;
            *stream = Some(rtcp_read_stream);
        }

        {
            let srtp_session = self.rtp_transport.get_srtp_session().await;
            let mut session = self.rtp_write_session.lock().await;
            *session = srtp_session;
        }

        Ok(())
    }

    pub async fn close(&self) -> Result<()> {
        let stream = self.rtcp_read_stream.lock().await;
        if let Some(rtcp_read_stream) = &*stream {
            Ok(rtcp_read_stream.close().await?)
        } else {
            Ok(())
        }
    }

    pub async fn read(&self, b: &mut [u8]) -> Result<usize> {
        {
            let stream = self.rtcp_read_stream.lock().await;
            if let Some(rtcp_read_stream) = &*stream {
                return Ok(rtcp_read_stream.read(b).await?);
            }
        }

        self.init(false).await?;

        {
            let stream = self.rtcp_read_stream.lock().await;
            if let Some(rtcp_read_stream) = &*stream {
                return Ok(rtcp_read_stream.read(b).await?);
            }
        }

        Ok(0)
    }

    pub async fn write_rtp(&self, pkt: &rtp::packet::Packet) -> Result<usize> {
        {
            let session = self.rtp_write_session.lock().await;
            if let Some(rtp_write_session) = &*session {
                return Ok(rtp_write_session.write_rtp(pkt).await?);
            }
        }

        self.init(true).await?;

        {
            let session = self.rtp_write_session.lock().await;
            if let Some(rtp_write_session) = &*session {
                return Ok(rtp_write_session.write_rtp(pkt).await?);
            }
        }

        Ok(0)
    }

    pub async fn write(&self, b: &Bytes) -> Result<usize> {
        {
            let session = self.rtp_write_session.lock().await;
            if let Some(rtp_write_session) = &*session {
                return Ok(rtp_write_session.write(b, true).await?);
            }
        }

        self.init(true).await?;

        {
            let session = self.rtp_write_session.lock().await;
            if let Some(rtp_write_session) = &*session {
                return Ok(rtp_write_session.write(b, true).await?);
            }
        }

        Ok(0)
    }
}

type IResult<T> = std::result::Result<T, interceptor::Error>;

#[async_trait]
impl RTCPReader for SrtpWriterFuture {
    async fn read(&self, buf: &mut [u8], a: &Attributes) -> IResult<(usize, Attributes)> {
        Ok((self.read(buf).await?, a.clone()))
    }
}

#[async_trait]
impl RTPWriter for SrtpWriterFuture {
    async fn write(&self, pkt: &rtp::packet::Packet, _a: &Attributes) -> IResult<usize> {
        Ok(self.write_rtp(pkt).await?)
    }
}
