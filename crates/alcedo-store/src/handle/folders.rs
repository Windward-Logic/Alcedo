//! `StoreHandle` methods for IMAP folders.

use super::*;
use crate::folders;

impl StoreHandle {
    pub async fn replace_folders(
        &self,
        account: AccountId,
        folders: &[Folder],
    ) -> Result<Vec<Folder>> {
        self.replace_folders_with_mode(account, folders, None).await
    }

    pub async fn replace_folders_with_mode(
        &self,
        account: AccountId,
        folders: &[Folder],
        mode: Option<folders::FolderReplaceMode>,
    ) -> Result<Vec<Folder>> {
        send_write(&self.write_tx, |resp| WriteRequest::ReplaceFolders {
            account,
            folders: folders.to_vec(),
            mode,
            resp,
        })
        .await
    }

    pub async fn set_folder_unread(&self, folder: FolderId, unread: u32) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::SetFolderUnread {
            folder,
            unread,
            resp,
        })
        .await
    }

    pub async fn list_folders(&self, account: AccountId) -> Result<Vec<Folder>> {
        send_read(&self.read_tx, |resp| ReadRequest::ListFolders {
            account,
            resp,
        })
        .await
    }

    pub async fn get_folder(&self, id: FolderId) -> Result<Option<Folder>> {
        send_read(&self.read_tx, |resp| ReadRequest::GetFolder { id, resp }).await
    }

    pub async fn rename_folder_tree(
        &self,
        account: AccountId,
        pairs: &[(String, String)],
    ) -> Result<()> {
        send_write(&self.write_tx, |resp| WriteRequest::RenameFolderTree {
            account,
            pairs: pairs.to_vec(),
            resp,
        })
        .await
    }
}
