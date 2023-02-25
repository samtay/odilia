use atspi::{
	accessible::{Accessible, AccessibleProxy, ObjectPair, Role},
	convertable::Convertable,
	text_ext::TextExt,
	AccessibleId, InterfaceSet, StateSet,
};
use odilia_common::{errors::{AccessiblePrimitiveConversionError, OdiliaError}, result::OdiliaResult};
use serde::{Deserialize, Serialize};
use std::{
	collections::{HashMap, hash_map::Entry},
	sync::{Arc, Weak},
};
use tokio::sync::{Mutex};
use zbus::{
	names::OwnedUniqueName,
	zvariant::{ObjectPath, OwnedObjectPath},
	ProxyBuilder,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
/// A struct representing an accessible. To get any information from the cache other than the stored information like role, interfaces, and states, you will need to instantiate an [`atspi::accessible::AccessibleProxy`] or other `*Proxy` type from atspi to query further info.
pub struct CacheItem {
	// the accessible ID from the path: /org/a11y/atspi/accessible/ID
	pub id: AccessibleId,
	// the sender, usually an X11 window ID.
	pub sender: String,
	// The application (root object(?)	  (so)
	pub app: AccessibleId,
	// The parent object.  (so)
	pub parent: CacheRef,
	// The accessbile index in parent.	i
	// TODO usize
	pub index: i32,
	// Children count
	// TODO usize
	pub child_count: i32,
	// Children
	pub children: Vec<CacheRef>,
	// The exposed interfece(s) set.  as
	pub ifaces: InterfaceSet,
	// Accessible role. u
	pub role: Role,
	// The states applicable to the accessible.  au
	pub states: StateSet,
	// The text of the accessible.
	pub text: String,
}

impl TryInto<ObjectPair> for CacheItem {
	type Error = OdiliaError;
	fn try_into(self) -> OdiliaResult<ObjectPair> {
		Ok((self.sender, self.id))
	}
}

/// A composition of an accessible ID and (possibly) a reference
/// to its `CacheItem`, if the item has not been dropped from the cache yet.
/// TODO if desirable, we could make one direction strong references (e.g. have
/// the parent be an Arc, or have the children be Arcs).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CacheRef {
	// TODO this gets changed to AccessibleId after #86
	id: AccessibleId,
	#[serde(skip)]
	item: Weak<CacheItem>,
}

/// Given a cache item `C`, find its parent and children and populate them with
/// the weak ref to `C`.
async fn make_cache_links(cache: Arc<Cache>, item: Arc<CacheItem>) {
	let self_id = item.id;
	let weak_ref = Arc::downgrade(&item);
	for child_ref in &item.children {
		cache.modify_item(&child_ref.id, |child| {
			child.parent.item = weak_ref.clone();
		})
		.await;
	}
	cache.modify_item(&item.parent.id, |parent| {
		if let Some(c) = parent.children.iter_mut().find(|c| c.id == self_id) {
			c.item = weak_ref;
		}
	})
	.await;
}

impl CacheRef {
	pub fn new(id: AccessibleId) -> Self {
		Self { id, item: Weak::new() }
	}
}

/// The root of the accessible cache.
//#[derive(Clone)]
pub struct Cache {
    // TODO condvar ? something that prioritizes bulk writes but lets single reads through?
    // TODO could Mutex the inner cache items... this would allow Weak refs to stay valid
    by_id: Mutex<HashMap<AccessibleId, Arc<CacheItem>>>
}
// clippy wants this
impl Default for Cache {
	fn default() -> Self {
		Self::new()
	}
}

/// An internal cache used within Odilia.
/// This contains (mostly) all accessibles in the entire accessibility tree, and they are referenced by their IDs.
/// When setting or getting information from the cache, be sure to use the most appropriate function.
/// For example, you would not want to remove individual items using the `remove()` function.
/// You should use the `remove_all()` function to acheive this, since this will only lock the cache mutex once, remove all ids, then refresh the cache.
/// If you are having issues with incorrect or invalid accessibles trying to be accessed, this is code is probably the issue.
/// This implementation is not very efficient, but it is very safe:
/// This is because before inserting, the incomming bucket is cleared (there will never be duplicate accessibles or accessibles at different states stored in the same bucket).
impl Cache {
	/// create a new, fresh cache
	pub fn new() -> Self {
		Self { by_id: Mutex::new(HashMap::new()) }
	}
	/// add a single new item to the cache. Note that this will empty the bucket before inserting the `CacheItem` into the cache (this is so there is never two items with the same ID stored in the cache at the same time).
	pub async fn add(&self, cache_item: Arc<CacheItem>) {
		let mut cache = self.by_id.lock().await;
		cache.insert(cache_item.id, cache_item);
	}
	/// remove a single cache item
	pub async fn remove(&self, id: &AccessibleId) {
		let mut cache_writer = self.by_id.lock().await;
		cache_writer.remove(id);
	}
	/// get a single item from the cache (note that this copies some integers to a new struct)
	#[allow(dead_code)]
	pub async fn get(&self, id: &AccessibleId) -> Option<Arc<CacheItem>> {
		let read_handle = self.by_id.lock().await;
		read_handle.get(id).cloned()
	}
    /// get a many items from the cache; this only creates one read handle (note
    /// that this will copy all data you would like to access)
	#[allow(dead_code)]
	pub async fn get_all(&self, ids: Vec<AccessibleId>) -> Vec<Option<Arc<CacheItem>>> {
		let read_handle = self.by_id.lock().await;
		ids.iter()
			.map(|id| read_handle.get(id).cloned())
			.collect()
	}
    /// Bulk add many items to the cache; this only refreshes the cache after
    /// adding all items. Note that this will empty the bucket before inserting.
    /// Only one accessible should ever be associated with an id.
	pub async fn add_all(&self, cache_items: Vec<CacheItem>) {
		let mut cache_writer = self.by_id.lock().await;
		cache_items.into_iter().for_each(|cache_item| {
			cache_writer.insert(cache_item.id, Arc::new(cache_item));
		});
	}
	/// Bulk remove all ids in the cache; this only refreshes the cache after removing all items.
	#[allow(dead_code)]
	pub async fn remove_all(&self, ids: Vec<AccessibleId>) {
		let mut cache_writer = self.by_id.lock().await;
		ids.iter().for_each(|id| {
			cache_writer.remove(id);
		});
	}

	/// Edit a mutable CacheItem using a function which returns the edited version.
    /// Note: an exclusive lock will be placed for the entire length of the
    /// passed function, so don't do any compute in it.
    ///
    /// Note: using this function destroys any weak refs to the entry :/
    ///
	/// Returns true if the update was successful.
	pub async fn modify_item<F>(&self, id: &AccessibleId, modify: F) -> bool
	where
		F: FnOnce(&mut CacheItem),
	{
		let mut cache = self.by_id.lock().await;
		let cache_item = match cache.entry(*id) {
            Entry::Occupied(i) => i.remove(),
			_ => {
				tracing::trace!(
					"The cache has the following items: {:?}",
					cache.keys()
				);
				return false;
			}
		};
        let mut cache_item = unwrap_or_clone(cache_item);
		modify(&mut cache_item);
        cache.insert(*id, Arc::new(cache_item));
		true
	}

    /// get a single item from the cache (note that this copies some integers to a new struct).
	/// If the CacheItem is not found, create one, add it to the cache, and return it.
	pub async fn get_or_create(self: Arc<Self>, accessible: &AccessibleProxy<'_>) -> OdiliaResult<Arc<CacheItem>> {
		// if the item already exists in the cache, return it
		if let Some(cache_item) = self.get(&accessible.accessible_id().await?).await {
			return Ok(cache_item);
		}
		// otherwise, build a cache item
		let start = std::time::Instant::now();
		let cache_item = Arc::new(accessible_to_cache_item(accessible).await?);
		let end = std::time::Instant::now();
		let diff = end - start;
		println!("Time to create cache item: {:?}", diff);
		// add a clone of it to the cache
		// Links to this item from others are not critical to the caller, so
		// spawn this off for (possibly) another thread.
		tokio::spawn(make_cache_links(self.clone(), cache_item.clone()));
		self.add(cache_item.clone()).await;
		// return that same cache item
		Ok(cache_item)
	}
}

// TODO we have access to parent and children AccessibleProxy's right here...
// should we populate them? Like a depth first? Seems... bad. Should we shoot
// off a tokio task to populate them?
pub async fn accessible_to_cache_item(accessible: &AccessibleProxy<'_>) -> OdiliaResult<CacheItem> {
	let (id, app, parent, index, child_count, ifaces, role, states, text, children) = tokio::try_join!(
		accessible.accessible_id(),
		Accessible::get_application(accessible),
		Accessible::parent(accessible),
		accessible.get_index_in_parent(),
		accessible.child_count(),
		accessible.get_interfaces(),
		accessible.get_role(),
		accessible.get_state(),
		accessible.name(),
		Accessible::get_children(accessible),
	)?;
	let sender = accessible.destination().to_string();
	let child_ids = children
		.iter()
		.map(|l| CacheRef::new(l.path().try_into().unwrap()))
		.collect();
	Ok(CacheItem {
		id,
		sender,
		app: app.accessible_id().await?,
		parent: CacheRef::new(parent.accessible_id().await?),
		index,
		child_count,
		children: child_ids,
		ifaces,
		role,
		states,
		text,
	})
}

pub fn unwrap_or_clone<T: Clone>(this: Arc<T>) -> T {
    Arc::try_unwrap(this).unwrap_or_else(|arc| (*arc).clone())
}
