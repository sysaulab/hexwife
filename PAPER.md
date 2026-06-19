# Hexwife: A Terminal Hex Viewer for Theoretical File System Limits

**Abstract**

Hexwife is a terminal-based hex viewer written in Rust, designed to gracefully handle the theoretical limits of operating systems. It uses 128-bit arithmetic for all offset and line calculations, enabling correct display and navigation of files up to \(2^{128}-1\) bytes. The viewer pulls data on demand with no internal caching, supports configurable byte groupings, a scrollbar, mouse input, and a parallel SIMD-accelerated search with progress ETA. The tool was stress‑tested against a 9 EB generative FUSE filesystem running over MacFuse, demonstrating both correctness and practical performance at extreme scale.

## 1. Introduction

Hex editors and viewers are essential for low‑level data inspection, but most assume that file sizes fit comfortably within 64‑bit address spaces. While no mainstream filesystem currently exceeds \(2^{64}\) bytes, theoretical limits exist, and synthetic filesystems can expose those boundaries for testing. **Hexwife** was built to handle those theoretical maxima without compromise: every offset, line number, and cursor position is stored and computed using 128‑bit integers. Coupled with a demand‑driven I/O model and SIMD‑accelerated search, the tool remains lightweight and responsive even when faced with exabyte‑scale files.

## 2. Design and Architecture

Hexwife is a single‑file Rust application using `ratatui` and `crossterm` for the terminal UI. The core principles are:

- **No caching** – only the visible portion of the file is read on every redraw. This keeps memory usage constant and leverages the operating system’s own page cache.
- **Dynamic layout** – the number of bytes displayed per line adjusts automatically to the terminal width, respecting a user‑selected grouping size (1, 2, 4, or 8 bytes).
- **Configurable grouping** – pressing `1`–`4` changes how bytes are clustered in the hex display, and the word interpretation below the cursor switches accordingly between u8, u16, u32, and u64 (both little‑ and big‑endian).
- **Scrollbar and mouse** – a vertical scrollbar indicates position within the file, and the mouse wheel moves the cursor by one line at a time.
- **Blocking SIMD search** – a `/` prompt accepts hex or ASCII patterns. The search reads 16 MiB blocks sequentially, using the `memchr` crate (which employs AVX2/SSE4.2 on x86‑64 and NEON on AArch64) for fast substring matching. Overlap between blocks prevents missed matches at boundaries. A live ETA is calculated and displayed.

## 3. 128‑Bit Internals

The central design decision is the exclusive use of `u128` for all file offsets. A file’s length, the cursor position, the first visible line index, and the byte range of the viewport are all 128‑bit values. Critical arithmetic, such as computing the starting offset of a line (`scroll_line * bytes_per_line`) or advancing the search offset, is done with `u128` saturating operations.

The address column in the hex dump is dynamically sized: the number of hex digits equals `⌊log₁₆(file_size - 1)⌋ + 2` (one extra digit for headroom). For a 9 EB file (\(9 \times 2^{60}\) bytes) this yields 17 hex digits per address. The viewport layout calculation also uses 128‑bit arithmetic to ensure no overflow when determining how many groups fit in the terminal width.

When I/O is required, the 128‑bit offset is converted to a 64‑bit `u64` via `u128_to_u64_safe()`, returning `None` for offsets beyond \(2^{64}-1\). Since no present OS can `seek` past that boundary, the viewer treats such regions as empty – a graceful degradation that avoids panics while remaining mathematically consistent.

## 4. Testing with a 9 EB Generative Filesystem

To validate the tool’s behaviour at extreme scale, a 9 exabyte sparse file was created using a generative FUSE filesystem written in Rust. The filesystem responds to `read` and `seek` calls by synthesising data on the fly, mimicking a real file of that size. Initial tests used FUSE‑T (a user‑space FUSE implementation that avoids kernel modules), but its high overhead resulted in a search ETA of roughly half a million years for a full scan. Switching to MacFuse reduced the overhead by a factor of 100, bringing the ETA down to ~5000 years – still far from practical, but demonstrating that the bottleneck is entirely I/O bandwidth, not the viewer’s logic.

The following aspects were verified:

- The 17‑digit address column correctly displays offsets for all visible lines, even when scrolling near the theoretical end of the file.
- Cursor navigation (arrows, `s`/`e` for start/end, `u`/`d` for page up/down) works without overflow or truncation.
- Searching for a known byte pattern placed at various offsets (including 1 GB and beyond) succeeds, thanks to the block‑overlap logic.
- The search ETA display scales correctly: for a full 9 EB scan, the ETA showed values on the order of thousands of years, while for small searches it dropped to seconds.
- Mouse wheel scrolling and the scrollbar track the viewport accurately, with the thumb position reflecting the true progress through the massive file.

## 5. Performance and Observations

The decision to forego internal caching keeps the viewer’s memory footprint constant (~20 MiB for the UI and search buffers) regardless of file size. The `memchr`‑based search processes approximately 2–4 GiB/s on modern NVMe storage; when combined with the FUSE overhead, the observed throughput was around 200 MiB/s under MacFuse. The ETA formula uses elapsed wall‑clock time and bytes scanned, providing an honest estimate of completion time that adapts to the actual I/O speed.

Despite the 128‑bit arithmetic, the compiler generates efficient code: on 64‑bit CPUs, `u128` operations are lowered to pairs of 64‑bit instructions with only minor overhead compared to native 64‑bit types. Profiling should show no measurable performance penalty from the wider integer sizes.

## 6. Conclusion

Hexwife demonstrates that it is possible to build a practical, user‑friendly hex viewer that remains mathematically correct up to the absolute limits of file addressing. By embracing 128‑bit internals, demand‑driven I/O, and SIMD search, the tool handles synthetic exabyte files as gracefully as it does small configuration files. The testing with a 9 EB FUSE filesystem validates both the correctness of the address calculations and the robustness of the I/O model, providing confidence that the viewer would function identically on real filesystems of comparable size, should they ever appear.

The source code is available as a single, easily auditable Rust file, embodying the Unix philosophy of doing one thing well – and doing it without compromise.