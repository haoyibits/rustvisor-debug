//! `probecore::fdx` against Lauterbach's FDX target agent
//! (`vendor/t32fdx.c`). No API: the tests are the crate.
//!
//! - The debugger sending: whatever `fdx::send` accepts, the agent's
//!   `T32_Fdx_ReceivePoll` reads back unchanged and in order, and a packet
//!   of `max_payload` bytes always fits once the agent has read everything.
//! - The target sending: what the agent's `T32_Fdx_SendPoll` writes,
//!   `fdx::receive` reads back unchanged and in order, with the agent's
//!   drop marker where it dropped packets.
//!
//! The only `unsafe` is the two calls into the C agent.
#![deny(unsafe_code)]
#![warn(clippy::undocumented_unsafe_blocks)]

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::ffi::{c_int, c_void};

    use probecore::fdx::{self, Channel, HEADER, Packet};
    use probecore::{Memory, Result};

    #[allow(unsafe_code)]
    unsafe extern "C" {
        fn T32_Fdx_SendPoll(channel: *mut c_void, buf: *const c_void, size: c_int) -> c_int;
        fn T32_Fdx_ReceivePoll(channel: *mut c_void, buf: *mut c_void, size: c_int) -> c_int;
    }

    /// Data bytes of the channel under test.
    const N: usize = 96;
    const WORDS: usize = (HEADER + N) / 4;

    #[cfg(not(target_endian = "little"))]
    compile_error!("the agent's data bytes are read back as little-endian words");

    /// `T32_Fdx_DefineChannel(c, N)`, initialised and enabled as the agent's
    /// macros do: header word 1 (`size`) is `N`, word 7 (`transferchannel`)
    /// is 1. Words, so that safe code reads its bytes; on a little-endian
    /// host data byte `i` is byte `i % 4` of word `8 + i / 4`, as the C code
    /// addresses it.
    #[repr(C, align(64))]
    struct Agent {
        words: [u32; WORDS],
    }

    impl Agent {
        fn new() -> Box<Self> {
            let mut words = [0; WORDS];
            words[1] = N as u32;
            words[7] = 1;
            Box::new(Agent { words })
        }

        fn channel(&self) -> Channel {
            Channel {
                address: 0,
                size: N,
            }
        }

        fn bytes(&self) -> Vec<u8> {
            self.words.iter().flat_map(|w| w.to_le_bytes()).collect()
        }

        #[allow(unsafe_code)]
        fn send(&mut self, payload: &[u8]) -> bool {
            // SAFETY: `self` is laid out as the agent's channel macro defines
            // it (eight 32-bit header words, then N data bytes), and the
            // payload pointer is valid for the length passed. The agent only
            // touches the channel and the payload during the call.
            let sent = unsafe {
                T32_Fdx_SendPoll(
                    (self as *mut Self).cast(),
                    payload.as_ptr().cast(),
                    payload.len() as c_int,
                )
            };
            sent > 0
        }

        #[allow(unsafe_code)]
        fn receive(&mut self) -> Option<Vec<u8>> {
            let mut buf = [0u8; N];
            // SAFETY: as in `send`, with a buffer of the length passed.
            let len = unsafe {
                T32_Fdx_ReceivePoll(
                    (self as *mut Self).cast(),
                    buf.as_mut_ptr().cast(),
                    N as c_int,
                )
            };
            (len > 0).then(|| buf[..len as usize].to_vec())
        }
    }

    /// The agent's channel as the debugger sees it, from address 0.
    impl Memory for Agent {
        fn read(&mut self, address: u64, length: usize) -> Result<Vec<u8>> {
            let at = address as usize;
            Ok(self.bytes()[at..at + length].to_vec())
        }

        fn read_u32s(&mut self, address: u64, count: usize) -> Result<Vec<u32>> {
            assert_eq!(address % 4, 0, "unaligned word read");
            let at = address as usize / 4;
            Ok(self.words[at..at + count].to_vec())
        }

        fn write(&mut self, _: u64, _: &[u8]) -> Result<()> {
            panic!("FDX writes only whole words");
        }

        fn write_u32(&mut self, address: u64, value: u32) -> Result<()> {
            assert_eq!(address % 4, 0, "unaligned word write");
            self.words[address as usize / 4] = value;
            Ok(())
        }
    }

    /// A deterministic sequence of lengths and read points, without a
    /// random-number crate.
    fn script(seed: u32, longest: usize) -> impl Iterator<Item = (usize, bool)> {
        let mut state = seed;
        std::iter::from_fn(move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            Some((1 + state as usize % longest, state & 0x300 == 0))
        })
    }

    #[test]
    fn what_the_debugger_sends_the_agent_receives() {
        let longest = fdx::max_payload(N);
        for seed in 1..60 {
            let mut agent = Agent::new();
            let channel = agent.channel();
            let mut expected = VecDeque::new();
            for (step, (len, read)) in script(seed, longest).take(400).enumerate() {
                let packet: Vec<u8> = (0..len).map(|i| (step * 3 + i) as u8).collect();
                if fdx::send(&mut *agent, &channel, &packet).unwrap() {
                    expected.push_back(packet);
                }
                if read {
                    while let Some(received) = agent.receive() {
                        assert_eq!(
                            Some(received),
                            expected.pop_front(),
                            "seed {seed} step {step}"
                        );
                    }
                    assert!(expected.is_empty(), "seed {seed} step {step}");
                    // Once everything is read, the longest packet fits.
                    let full = vec![0xA5; longest];
                    assert!(
                        fdx::send(&mut *agent, &channel, &full).unwrap(),
                        "seed {seed} step {step}: no room for {longest} bytes in an empty ring"
                    );
                    assert_eq!(agent.receive(), Some(full), "seed {seed} step {step}");
                }
            }
        }
    }

    #[test]
    fn what_the_agent_sends_the_debugger_receives() {
        let longest = fdx::max_payload(N);
        for seed in 1..60 {
            let mut agent = Agent::new();
            let channel = agent.channel();
            let mut expected = VecDeque::new();
            let mut failed = false;
            for (step, (len, read)) in script(seed, longest).take(400).enumerate() {
                let packet: Vec<u8> = (0..len).map(|i| (step + i) as u8 | 1).collect();
                if agent.send(&packet) {
                    expected.push_back(packet);
                } else {
                    failed = true;
                }
                if read {
                    while let Some(received) = fdx::receive(&mut *agent, &channel).unwrap() {
                        match received {
                            Packet::Data(data) => assert_eq!(
                                Some(data),
                                expected.pop_front(),
                                "seed {seed} step {step}"
                            ),
                            Packet::Dropped => {
                                assert!(failed, "seed {seed} step {step}: a drop without one")
                            }
                        }
                    }
                    assert!(expected.is_empty(), "seed {seed} step {step}");
                    failed = false;
                }
            }
        }
    }

    #[test]
    fn a_dropped_packet_reads_as_the_agent_marks_it() {
        let mut agent = Agent::new();
        let channel = agent.channel();
        while agent.send(&[1; 20]) {}
        let mut packets = Vec::new();
        while let Some(packet) = fdx::receive(&mut *agent, &channel).unwrap() {
            packets.push(packet);
        }
        assert_eq!(packets.last(), Some(&Packet::Dropped));
        assert!(
            packets[..packets.len() - 1]
                .iter()
                .all(|p| *p == Packet::Data(vec![1; 20]))
        );
    }

    #[test]
    fn a_closed_agent_channel_reads_as_closed() {
        let mut agent = Agent::new();
        assert_eq!(fdx::opened(&mut *agent, 0), Ok(Some(N)));
        agent.words[1] = 0;
        assert_eq!(fdx::opened(&mut *agent, 0), Ok(None));
        assert_eq!(
            fdx::receive(
                &mut *agent,
                &Channel {
                    address: 0,
                    size: N
                }
            ),
            Err(fdx::Error::Closed)
        );
    }
}
