use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;

use futures::{executor::block_on, stream};
use mtp_rs::{MtpDevice, MtpDeviceInfo, NewObjectInfo, ObjectHandle, ObjectInfo, Storage};

use super::{
    prepare_book, Device, DeviceKind, PreparedBook, SendOptions, SendReport, SentBook, Transport,
};
use crate::error::{Error, Result};
use crate::library::Library;

const AMAZON: u16 = 0x1949;
// The Paperwhite, Scribe and Colorsoft can share this vendor-specific USB ID.
const KINDLE_IDS: &[(u16, u16)] = &[(AMAZON, 0x9981)];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Kindle {
    location: u64,
    product: u16,
    serial: Option<String>,
}

fn kindle_info() -> Result<impl Iterator<Item = MtpDeviceInfo>> {
    let devices = MtpDevice::list_devices_with_known(KINDLE_IDS)
        .map_err(|err| Error::Device(format!("Could not scan USB for Kindles: {err}")))?;
    Ok(devices.into_iter().filter(|info| {
        KINDLE_IDS.contains(&(info.vendor_id, info.product_id))
            || (info.vendor_id == AMAZON
                && info.product.as_deref().is_some_and(|name| {
                    name.starts_with("Kindle") && !name.starts_with("Kindle Fire")
                }))
    }))
}

pub(super) fn discover() -> Result<Vec<Device>> {
    Ok(kindle_info()?
        .map(|info| Device {
            root: PathBuf::from(format!("mtp:{:016x}", info.location_id)),
            name: info.product.unwrap_or_else(|| "Kindle".to_string()),
            kind: DeviceKind::Kindle,
            transport: Transport::Mtp(Kindle {
                location: info.location_id,
                product: info.product_id,
                serial: info.serial_number,
            }),
        })
        .collect())
}

pub(super) fn send(
    library: &Library,
    device: &Device,
    kindle: &Kindle,
    ids: &[i64],
    options: &SendOptions,
) -> Result<SendReport> {
    let mut report = SendReport {
        dry_run: options.dry_run,
        ..Default::default()
    };
    let mut books = Vec::new();
    for &id in ids {
        if let Some(book) = prepare_book(library, device, id, options, &mut report)? {
            books.push((id, book));
        }
    }
    if books.is_empty() {
        return Ok(report);
    }

    let still_connected = kindle_info()?.any(|info| {
        info.location_id == kindle.location
            && info.product_id == kindle.product
            && info.serial_number == kindle.serial
    });
    if !still_connected {
        return Err(Error::Device(format!(
            "{} disconnected. Reconnect it in USB file-transfer mode and scan again.",
            device.name
        )));
    }

    block_on(async {
        let connection = MtpDevice::builder()
            .known_devices(KINDLE_IDS)
            .open_by_location(kindle.location)
            .await
            .map_err(|err| {
                Error::Device(format!(
                    "Could not open {}. Unlock the Kindle and close other USB file-transfer apps. ({err})",
                    device.name
                ))
            })?;
        let transfer = upload_books(&connection, device, books, &mut report).await;
        let close = connection.close().await;
        transfer?;
        close.map_err(|err| Error::Device(format!("Could not close Kindle session: {err}")))?;
        Ok(report)
    })
}

async fn upload_books(
    connection: &MtpDevice,
    device: &Device,
    books: Vec<(i64, PreparedBook)>,
    report: &mut SendReport,
) -> Result<()> {
    let (storage, folder) = documents_folder(connection).await?;
    let listing = storage
        .collect_objects(Some(folder.handle))
        .await
        .map_err(|err| Error::Device(format!("Could not list Kindle documents: {err}")))?;
    if !listing.skipped.is_empty() {
        return Err(Error::Device(
            "Could not read all Kindle filenames.".to_string(),
        ));
    }
    let mut names: HashSet<_> = listing
        .objects
        .into_iter()
        .map(|file| file.filename.to_lowercase())
        .collect();
    for (id, book) in books {
        // Leave room for the extension, collision suffix and folder name.
        let mut counter = 1;
        let mut filename = book.file_name(counter, Some(210));
        while !names.insert(filename.to_lowercase()) {
            counter += 1;
            filename = book.file_name(counter, Some(210));
        }
        let file = File::open(&book.source)?;
        let size = file.metadata()?.len();
        if !report.dry_run {
            upload_file(&storage, folder.handle, &filename, size, file).await?;
        }
        report.sent.push(SentBook {
            id,
            title: book.meta.title,
            format: book.format,
            path: device.root.join(&folder.filename).join(filename),
            bytes: size,
        });
    }
    Ok(())
}

async fn upload_file(
    storage: &Storage,
    parent: ObjectHandle,
    filename: &str,
    size: u64,
    file: File,
) -> Result<()> {
    let info = NewObjectInfo::file(filename, size);
    if let Err(err) = storage.upload(Some(parent), info, file_chunks(file)).await {
        if let Some(handle) = err.partial {
            storage.delete(handle).await.map_err(|cleanup| {
                Error::Device(format!(
                    "Could not upload {filename}: {err}. Could not remove the partial file: {cleanup}"
                ))
            })?;
        }
        return Err(Error::Device(format!("Could not upload {filename}: {err}")));
    }
    Ok(())
}

async fn documents_folder(connection: &MtpDevice) -> Result<(Storage, ObjectInfo)> {
    let storages = connection
        .storages()
        .await
        .map_err(|err| Error::Device(format!("Could not read Kindle storage: {err}")))?;
    for storage in storages
        .into_iter()
        .filter(|storage| storage.info().is_writable)
    {
        let root = storage
            .list_objects(None)
            .await
            .map_err(|err| Error::Device(format!("Could not list Kindle storage: {err}")))?;
        if let Some(folder) = root
            .into_iter()
            .find(|item| item.is_folder() && item.filename.eq_ignore_ascii_case("documents"))
        {
            return Ok((storage, folder));
        }
    }
    Err(Error::Device(
        "No writable documents folder on the Kindle. Unlock it and enable USB file transfer."
            .to_string(),
    ))
}

fn file_chunks<B: From<Vec<u8>>>(
    mut file: File,
) -> impl stream::Stream<Item = io::Result<B>> + Unpin + Send {
    stream::iter(std::iter::from_fn(move || {
        let mut chunk = vec![0; 64 * 1024];
        match file.read(&mut chunk) {
            Ok(0) => None,
            Ok(count) => {
                chunk.truncate(count);
                Some(Ok(chunk.into()))
            }
            Err(err) => Some(Err(err)),
        }
    }))
}
