//! Writes the test archives the probe reads, into `corpus/` (or the folder
//! given): hostile names and links, a zip bomb, names in old code pages, 7z
//! with and without a hidden file list, and a 7z split into parts with some
//! missing. Everything is generated, so nothing hostile is checked in.

mod raw;

use std::fs;
use std::io::Cursor;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use encoding_rs::Encoding;
use sevenz_rust2::encoder_options::AesEncoderOptions;
use sevenz_rust2::{ArchiveEntry, ArchiveWriter, EncoderMethod, Password};
use unicode_normalization::UnicodeNormalization;

use raw::{Body, Meta, Tar, Zip};

pub const PASSWORD: &str = "spike-pass";

fn main() -> Result<()> {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| "corpus".into()));
    for dir in ["hostile", "encoding", "7z/volumes"] {
        fs::create_dir_all(out.join(dir))?;
    }
    hostile(&out.join("hostile"))?;
    encodings(&out.join("encoding"))?;
    seven_zip(&out.join("7z"))?;
    println!("corpus written to {}", out.display());
    Ok(())
}

fn write(path: &Path, bytes: &[u8]) -> Result<()> {
    fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

fn hostile(dir: &Path) -> Result<()> {
    let body = Body::stored(b"if you can read this outside the folder, the extractor failed\n");
    let mut zip = Zip::new();
    for name in [
        "../evil.txt",
        "..\\evil-backslash.txt",
        "/abs.txt",
        "C:\\Windows\\win.txt",
        "ok/../../up.txt",
        "good/inside.txt",
        "doc.txt:Zone.Identifier",
        "CON.txt",
        "trailing. .",
        "dots/.../x.txt",
        "nul\0byte.txt",
        "a/.. /b.txt",
    ] {
        zip.add(name.as_bytes(), &body, Meta::utf8());
    }
    write(&dir.join("traversal.zip"), &zip.finish())?;

    // The classic: a link that points out, then a file written "into" it.
    let mut zip = Zip::new();
    zip.add(b"link", &Body::stored(b"../../outside"), Meta::symlink())
        .add(b"link/pwned.txt", &body, Meta::file())
        .add(b"abs", &Body::stored(b"/etc"), Meta::symlink())
        .add(b"sub/file.txt", &body, Meta::file())
        .add(b"inner", &Body::stored(b"sub/file.txt"), Meta::symlink());
    write(&dir.join("symlink-escape.zip"), &zip.finish())?;

    // 2 GiB of zeros in about 2 MB: past Deflate's 1032:1 at 1 GiB written.
    let mut zip = Zip::new();
    zip.add(b"zeros.bin", &Body::deflated_zeros(2 << 30)?, Meta::utf8());
    write(&dir.join("bomb.zip"), &zip.finish())?;

    // One 256 MiB block of zeros, listed 400 times: 100 GiB from 256 KB.
    let mut zip = Zip::new();
    zip.add(
        b"zeros-000.bin",
        &Body::deflated_zeros(256 << 20)?,
        Meta::utf8(),
    );
    for i in 1..400 {
        zip.alias(format!("zeros-{i:03}.bin").as_bytes(), 0);
    }
    write(&dir.join("overlap.zip"), &zip.finish())?;

    // Claims 1 KiB, inflates to 64 MiB.
    let mut zip = Zip::new();
    let meta = Meta {
        claimed_size: Some(1024),
        ..Meta::utf8()
    };
    zip.add(b"small.txt", &Body::deflated_zeros(64 << 20)?, meta);
    write(&dir.join("lying-size.zip"), &zip.finish())?;

    // An archive in an archive: the download mark must reach setup.exe too.
    let mut inner = Zip::new();
    inner.add(
        b"setup.exe",
        &Body::stored(b"MZ pretend program"),
        Meta::utf8(),
    );
    let mut outer = Zip::new();
    outer.add(b"inner.zip", &Body::stored(&inner.finish()), Meta::utf8());
    write(&dir.join("nested.zip"), &outer.finish())?;

    let mut tar = Tar::new();
    tar.add("link", b'2', "../../outside", b"")
        .add("link/pwned.txt", b'0', "", b"pwned\n")
        .add("hard", b'1', "/etc/passwd", b"")
        .add("../tar-evil.txt", b'0', "", b"evil\n")
        .add("ok/file.txt", b'0', "", b"fine\n")
        .add("ok/hard-inside", b'1', "ok/file.txt", b"");
    write(&dir.join("links.tar"), &tar.finish())?;
    Ok(())
}

/// One archive per code page, with names a person there would write. The
/// manifest gives each archive's names as they should read, and the web
/// domain of the place, which the probe can pass as a hint.
fn encodings(dir: &Path) -> Result<()> {
    let vi = [
        "Bản vẽ mặt bằng tầng 1.pdf",
        "Hợp đồng thi công – bản ký.docx",
        "Dự toán chi phí.xlsx",
        "Ghi chú họp ngày 14-10.txt",
        "Ảnh hiện trạng/Phối cảnh mặt tiền.jpg",
    ];
    let ja = [
        "見積書_2026年度.xlsx",
        "会議の議事録.txt",
        "写真/桜の木.jpg",
        "請求書（10月分）.pdf",
    ];
    let zh = [
        "项目报告.docx",
        "财务数据_2026.xlsx",
        "图片/风景照片.jpg",
        "会议记录.txt",
    ];
    let tw = [
        "專案報告.docx",
        "財務資料.xlsx",
        "圖片/風景.jpg",
        "會議記錄.txt",
    ];
    let ko = [
        "회의록.txt",
        "견적서_2026.xlsx",
        "사진/풍경.jpg",
        "계약서 최종본.pdf",
    ];
    let ru = [
        "Отчёт за квартал.doc",
        "Фото/Лето 2026.jpg",
        "Договор поставки.pdf",
    ];
    let latin = [
        "Résumé.docx",
        "Café menu.pdf",
        "Prévisions été.xlsx",
        "Señal.txt",
    ];
    let de_dos = [
        "Größe und Maße.txt",
        "Café menu.pdf",
        "Résumé.docx",
        "Año nuevo.jpg",
    ];

    let mut manifest = String::from("# archive\tcharset\ttld\tnames (|)\n");
    let mut case = |file: &str,
                    label: &str,
                    tld: &str,
                    names: &[&str],
                    encode: &dyn Fn(&str) -> Vec<u8>,
                    utf8_flag: bool|
     -> Result<()> {
        let mut zip = Zip::new();
        let body = Body::stored(b"x");
        for name in names {
            zip.add(
                &encode(name),
                &body,
                Meta {
                    utf8_flag,
                    ..Meta::default()
                },
            );
        }
        write(&dir.join(file), &zip.finish())?;
        manifest.push_str(&format!("{file}\t{label}\t{tld}\t{}\n", names.join("|")));
        Ok(())
    };
    let utf8 = |s: &str| s.as_bytes().to_vec();
    let nfd = |s: &str| s.nfd().collect::<String>().into_bytes();
    let with = |enc: &'static Encoding| {
        move |s: &str| {
            let (bytes, _, errors) = enc.encode(s);
            assert!(!errors, "{s} does not fit {}", enc.name());
            bytes.into_owned()
        }
    };

    case("utf8-flag.zip", "UTF-8", "-", &vi, &utf8, true)?;
    case("utf8-noflag.zip", "UTF-8", "-", &vi, &utf8, false)?;
    case("utf8-nfd-noflag.zip", "UTF-8", "-", &vi, &nfd, false)?;
    case(
        "vi-windows-1258.zip",
        "windows-1258",
        "vn",
        &vi,
        &vietnamese_1258,
        false,
    )?;
    case(
        "ja-shift_jis.zip",
        "Shift_JIS",
        "jp",
        &ja,
        &with(encoding_rs::SHIFT_JIS),
        false,
    )?;
    case(
        "zh-gbk.zip",
        "GBK",
        "cn",
        &zh,
        &with(encoding_rs::GBK),
        false,
    )?;
    case(
        "zh-big5.zip",
        "Big5",
        "tw",
        &tw,
        &with(encoding_rs::BIG5),
        false,
    )?;
    case(
        "ko-euc-kr.zip",
        "EUC-KR",
        "kr",
        &ko,
        &with(encoding_rs::EUC_KR),
        false,
    )?;
    case(
        "ru-ibm866.zip",
        "IBM866",
        "ru",
        &ru,
        &with(encoding_rs::IBM866),
        false,
    )?;
    case(
        "fr-windows-1252.zip",
        "windows-1252",
        "fr",
        &latin,
        &with(encoding_rs::WINDOWS_1252),
        false,
    )?;
    case(
        "de-cp437.zip",
        "IBM437",
        "de",
        &de_dos,
        &|s: &str| spike_names::encode_cp437(s).expect("cp437"),
        false,
    )?;
    write(&dir.join("expected.tsv"), manifest.as_bytes())
}

/// Vietnamese in Windows-1258, the way Windows writes it: the vowels with a
/// hat, breve or horn are single bytes (â, ơ, ư…) and the five tones follow
/// as separate combining bytes.
fn vietnamese_1258(text: &str) -> Vec<u8> {
    const TONES: [char; 5] = ['\u{300}', '\u{301}', '\u{303}', '\u{309}', '\u{323}'];
    let mut out = String::new();
    let mut cluster = String::new();
    let mut tones = String::new();
    let flush = |cluster: &mut String, tones: &mut String, out: &mut String| {
        out.extend(cluster.nfc());
        out.push_str(tones);
        cluster.clear();
        tones.clear();
    };
    for c in text.nfd() {
        if TONES.contains(&c) {
            tones.push(c);
        } else if unicode_normalization::char::is_combining_mark(c) {
            cluster.push(c);
        } else {
            flush(&mut cluster, &mut tones, &mut out);
            cluster.push(c);
        }
    }
    flush(&mut cluster, &mut tones, &mut out);
    let (bytes, _, errors) = encoding_rs::WINDOWS_1258.encode(&out);
    assert!(!errors, "{text} does not fit windows-1258");
    bytes.into_owned()
}

fn seven_zip(dir: &Path) -> Result<()> {
    let files: [(&str, Vec<u8>); 3] = [
        ("Hồ sơ khách hàng/Hợp đồng.docx", b"contract".repeat(500)),
        ("Hồ sơ khách hàng/Bảng lương.xlsx", b"salaries".repeat(500)),
        ("readme.txt", b"hello\n".to_vec()),
    ];
    let build = |password: Option<&str>, hide_names: bool, extra: &[u8]| -> Result<Vec<u8>> {
        let mut w = ArchiveWriter::new(Cursor::new(Vec::new()))?;
        if let Some(p) = password {
            w.set_content_methods(vec![
                AesEncoderOptions::new(Password::from(p)).into(),
                EncoderMethod::LZMA2.into(),
            ]);
        }
        w.set_encrypt_header(hide_names);
        for (name, data) in &files {
            w.push_archive_entry(ArchiveEntry::new_file(name), Some(Cursor::new(data)))?;
        }
        if !extra.is_empty() {
            w.push_archive_entry(
                ArchiveEntry::new_file("noise.bin"),
                Some(Cursor::new(extra)),
            )?;
        }
        Ok(w.finish()?.into_inner())
    };
    write(&dir.join("plain.7z"), &build(None, false, &[])?)?;
    write(
        &dir.join("names-visible.7z"),
        &build(Some(PASSWORD), false, &[])?,
    )?;
    write(
        &dir.join("names-hidden.7z"),
        &build(Some(PASSWORD), true, &[])?,
    )?;

    // Split the way 7-Zip splits: name.7z.001, .002…, cut at a byte count.
    let whole = build(None, false, &noise(3_500_000))?;
    let part = 1 << 20;
    for (set, skip) in [
        ("complete", None),
        ("missing-middle", Some(2)),
        ("missing-last", Some(4)),
    ] {
        let d = dir.join("volumes").join(set);
        fs::create_dir_all(&d)?;
        for (i, chunk) in whole.chunks(part).enumerate() {
            let n = i + 1;
            if Some(n) != skip {
                write(&d.join(format!("sample.7z.{n:03}")), chunk)?;
            }
        }
    }
    Ok(())
}

/// Bytes that do not compress, like photos or video.
fn noise(len: usize) -> Vec<u8> {
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 32) as u8
        })
        .collect()
}
