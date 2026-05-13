use std::collections::HashMap;

/// Utility functions for working with Matrix.
use matrix_sdk::{ruma::events::tag::TagInfo, Room};

/// Get all tags in a room that start with a given namespace.
/// Tags are supposed to be namespaced to the application in the form
/// of `tld.domain.tag`.
pub async fn get_tags(room: &Room, namespace: &str) -> Vec<String> {
    let mut all_tags = Vec::new();
    let tags = room.tags().await.unwrap_or_default();
    for (tag, _) in tags.unwrap_or_default() {
        if tag.to_string().starts_with(namespace) {
            let tag = tag.to_string();
            let tag = tag.replacen(&namespace.to_string(), "", 1);
            let tag = tag.trim_start_matches('.');
            all_tags.push(tag.to_string());
        }
    }
    all_tags
}

/// Adds a single tag to the room.
pub async fn add_tag(room: &Room, namespace: &str, tag: &str) -> Result<(), matrix_sdk::Error> {
    if !namespace.is_empty() {
        room.set_tag(format!("{}.{}", namespace, tag).into(), TagInfo::default())
            .await?;
    } else {
        room.set_tag(tag.to_string().into(), TagInfo::default())
            .await?;
    }
    Ok(())
}

/// Remove a namespaced tag from the room.
pub async fn remove_tag(room: &Room, namespace: &str, tag: &str) -> Result<(), matrix_sdk::Error> {
    if !namespace.is_empty() {
        room.remove_tag(format!("{}.{}", namespace, tag).into())
            .await?;
    } else {
        room.remove_tag(tag.to_string().into()).await?;
    }
    Ok(())
}

/// Set the tags for a room using a namespace.
/// These tags will replace any existing tags in the same namespace.
pub async fn replace_tags(room: &Room, namespace: &str, tags: &[String]) {
    let mut existing_tags = get_tags(room, namespace).await;
    // Remove tags that are in both the existing tags and the new tags.
    let mut tags = tags.to_owned();
    existing_tags.retain(|tag| !tags.contains(tag));
    tags.retain(|tag| !existing_tags.contains(tag));

    // Add tags that are in the new tags, and remove tags that are in the existing tags
    for tag in tags {
        add_tag(room, namespace, &tag).await.unwrap();
    }
    for tag in existing_tags {
        remove_tag(room, namespace, &tag).await.unwrap();
    }
}

/// The namespaced tags in a room.
///
/// This struct is an opinionated way to manage tags in a room.
/// It supports either setting raw tags or key-value tags.
///
/// Tags will only be synced when the struct is dropped or when sync() is called.
pub struct Tags<'a> {
    /// The namespace of the tags.
    /// Tags are supposed to be namespaced to the application in the form
    /// of `tld.domain.tag`.
    namespace: String,

    /// List of tags in the room.
    tags: Vec<String>,

    /// The room that the tags are associated with.
    room: &'a Room,

    /// Track whether the tags have been updated.
    /// This is used to determine whether to sync the tags with the server.
    dirty: bool,
}

impl<'a> Tags<'a> {
    /// Create a new Tags struct from a room and a namespace.
    ///
    /// The namespace is supposed to be in the form of `tld.domain`, and tags will be stored in `tld.domain.tag`.
    pub async fn new(room: &'a Room, namespace: &str) -> Self {
        let tags = get_tags(room, namespace).await;
        Self {
            namespace: namespace.to_string(),
            tags,
            room,
            dirty: false,
        }
    }

    /// Add a tag to the room.
    /// This will not sync the tags with the server until a sync() or the struct is dropped.
    pub fn add(&mut self, tag: &str) {
        self.tags.push(tag.to_string());
        self.dirty = true;
    }

    /// Get a value from a key.
    pub fn get_value(&self, key: &str) -> Option<String> {
        for tag in &self.tags {
            if tag.starts_with(&format!("{}=", key)) {
                return Some(tag.split('=').nth(1)?.to_string());
            }
        }
        None
    }

    /// Add a key-value tag to the room.
    /// It is added in the form of `key=value`.
    /// This will not sync the tags with the server until a sync() or the struct is dropped.
    pub fn add_kv(&mut self, key: &str, value: &str) {
        self.tags.push(format!("{}={}", key, value));
        self.dirty = true;
    }

    /// Replaces a key-value tag in the room with a new value.
    pub fn replace_kv(&mut self, key: &str, value: &str) {
        self.tags.retain(|t| !t.starts_with(&format!("{}=", key)));
        self.tags.push(format!("{}={}", key, value));
        self.dirty = true;
    }

    /// Removes an existing key-value tag if it exists.
    pub fn remove_kv(&mut self, key: &str) {
        self.tags.retain(|t| !t.starts_with(&format!("{}=", key)));
        self.dirty = true;
    }

    /// Remove a tag from the room.
    /// This will not sync the tags with the server until a sync() or the struct is dropped.
    /// If the tag is not in the room, this function will do nothing.
    pub fn remove(&mut self, tag: &str) {
        self.tags.retain(|t| t != tag);
        self.dirty = true;
    }

    /// Sync tags with the server.
    pub async fn sync(&mut self) {
        replace_tags(self.room, &self.namespace, &self.tags).await;
        self.dirty = false;
    }

    /// Get the namespace.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Get the tags.
    pub fn tags(&self) -> &Vec<String> {
        &self.tags
    }

    /// Get the KV pairs.
    pub fn get_kvs(&self) -> HashMap<String, String> {
        let mut kvs = HashMap::new();
        for tag in &self.tags {
            if let Some((key, value)) = tag.split_once('=') {
                kvs.insert(key.to_string(), value.to_string());
            }
        }
        kvs
    }

    /// Check if the tags will need to be synced.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }
}

// Implement the drop trait on Tags, which syncs the existing tags with the server
// when the struct is dropped.
#[allow(unused)]
impl<'a> Drop for Tags<'a> {
    fn drop(&mut self) {
        if self.dirty {
            let room = self.room.clone();
            let namespace = self.namespace.clone();
            let tags = self.tags.clone();
            tokio::spawn(async move {
                replace_tags(&room, &namespace, &tags).await;
            });
        }
    }
}
