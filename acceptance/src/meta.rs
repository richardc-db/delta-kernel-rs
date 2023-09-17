use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures::stream::TryStreamExt;
use object_store::{local::LocalFileSystem, ObjectStore};
use serde::{Deserialize, Serialize};
use url::Url;

use deltakernel::snapshot::Snapshot;
use deltakernel::{Error, Table, TableClient, Version};

#[derive(Debug, thiserror::Error)]
pub enum AssertionError {
    #[error("Invalid test case data")]
    InvalidTestCase,

    #[error("Kernel error: {0}")]
    KernelError(#[from] Error),
}

pub type TestResult<T, E = AssertionError> = std::result::Result<T, E>;

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
struct TestCaseInfoJson {
    name: String,
    description: String,
}

#[derive(PartialEq, Eq, Debug)]
pub struct TestCaseInfo {
    name: String,
    description: String,
    root_dir: PathBuf,
}

impl TestCaseInfo {
    /// Root path for this test cases Delta table.
    pub fn table_root(&self) -> TestResult<Url> {
        let table_root = self.root_dir.join("delta");
        Url::from_directory_path(table_root).map_err(|_| AssertionError::InvalidTestCase)
    }

    async fn versions(&self) -> TestResult<(TableVersionMetaData, Vec<TableVersionMetaData>)> {
        let expected_root = self.root_dir.join("expected");
        let store = LocalFileSystem::new_with_prefix(&expected_root).unwrap();

        let files = store
            .list(None)
            .await
            .unwrap()
            .try_collect::<Vec<_>>()
            .await
            .unwrap();

        let raw_cases = files.into_iter().filter(|meta| {
            meta.location.filename() == Some("table_version_metadata.json")
                && !meta
                    .location
                    .prefix_matches(&object_store::path::Path::from("latest"))
        });

        let mut cases = Vec::new();
        for case in raw_cases {
            let case_file = expected_root.join(case.location.as_ref());
            let file = File::open(case_file).map_err(|_| AssertionError::InvalidTestCase)?;
            let info: TableVersionMetaData =
                serde_json::from_reader(file).map_err(|_| AssertionError::InvalidTestCase)?;
            cases.push(info);
        }

        let case_file = expected_root.join("latest/table_version_metadata.json");
        let file = File::open(case_file).map_err(|_| AssertionError::InvalidTestCase)?;
        let latest: TableVersionMetaData =
            serde_json::from_reader(file).map_err(|_| AssertionError::InvalidTestCase)?;

        Ok((latest, cases))
    }

    async fn assert_snapshot_meta<JRC: Send, PRC: Send + Sync>(
        &self,
        case: &TableVersionMetaData,
        snapshot: &Snapshot<JRC, PRC>,
    ) -> TestResult<()> {
        assert_eq!(snapshot.version(), case.version);

        // assert correct metadata is read
        let metadata = snapshot.metadata().await?;
        let protocol = snapshot.protocol().await?;
        let tvm = TableVersionMetaData {
            version: snapshot.version(),
            properties: metadata
                .configuration
                .into_iter()
                .map(|(k, v)| (k, v.unwrap()))
                .collect(),
            min_reader_version: protocol.min_reader_version as u32,
            min_writer_version: protocol.min_writer_version as u32,
        };
        assert_eq!(&tvm, case);
        Ok(())
    }

    pub async fn assert_metadata<JRC: Send, PRC: Send + Sync>(
        &self,
        table_client: Arc<dyn TableClient<JsonReadContext = JRC, ParquetReadContext = PRC>>,
    ) -> TestResult<()> {
        let table = Table::new(self.table_root()?, table_client);

        let (latest, versions) = self.versions().await?;

        let snapshot = table.snapshot(None)?;
        self.assert_snapshot_meta(&latest, &snapshot).await?;

        for table_version in versions {
            let snapshot = table.snapshot(Some(table_version.version))?;
            self.assert_snapshot_meta(&table_version, &snapshot).await?;
        }

        Ok(())
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
pub struct TableVersionMetaData {
    version: Version,
    properties: HashMap<String, String>,
    min_reader_version: u32,
    min_writer_version: u32,
}

pub fn read_dat_case(case_root: impl AsRef<Path>) -> TestResult<TestCaseInfo> {
    let info_path = case_root.as_ref().join("test_case_info.json");
    let file = File::open(info_path).map_err(|_| AssertionError::InvalidTestCase)?;
    let info: TestCaseInfoJson =
        serde_json::from_reader(file).map_err(|_| AssertionError::InvalidTestCase)?;
    Ok(TestCaseInfo {
        root_dir: case_root.as_ref().into(),
        name: info.name,
        description: info.description,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_read_test_case() {
        let path = PathBuf::from("./tests/dat/out/reader_tests/generated/with_schema_change");
        let case = read_dat_case(path).unwrap();
        let versions = case.versions().await.unwrap();
        println!("{:?}", versions)
    }
}