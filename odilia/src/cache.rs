use rustc_hash::FxHasher;
use tokio::sync::Mutex;
use atspi::{
	accessible::Role,
	InterfaceSet,
	StateSet,
};
use evmap::ShallowCopy;
use std::mem::ManuallyDrop;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Copy)]
pub struct CacheItem {
    // The accessible object (within the application)   (so)
    pub object: i32,
    // The application (root object(?)    (so)
    pub app: i32,
    // The parent object.  (so)
    pub parent: i32,
    // The accessbile index in parent.  i
    pub index: i32,
    // Child count of the accessible  i
    pub children: i32,
    // The exposed interfece(s) set.  as
    pub ifaces: InterfaceSet,
    // Accessible role. u
    pub role: Role,
    // The states applicable to the accessible.  au
    pub states: StateSet,
}
impl ShallowCopy for CacheItem {
	unsafe fn shallow_copy(&self) -> ManuallyDrop<Self> {
		ManuallyDrop::new(*self)
	}
}

type FxBuildHasher = std::hash::BuildHasherDefault<FxHasher>;
pub type FxReadHandleFactory<K, V> = evmap::ReadHandleFactory<K, V, (), FxBuildHasher>;
pub type FxWriteHandle<K, V> = evmap::WriteHandle<K, V, (), FxBuildHasher>;
pub type FxReadGuard<'a, V> = evmap::ReadGuard<'a, V>;

/// The root of the accessible cache.
pub struct Cache {
    pub by_id_read: FxReadHandleFactory<i32, CacheItem>,
    pub by_id_write: Mutex<FxWriteHandle<i32, CacheItem>>,
}

/// Copy all info into a plain CacheItem struct.
/// This is very cheap, and the locking overhead will vastly outstrip making this a non-copy struct.
#[inline]
fn copy_into_cache_item(cache_item_with_handle: FxReadGuard<'_, CacheItem>) -> CacheItem {
	CacheItem {
		object: cache_item_with_handle.object,
		parent: cache_item_with_handle.parent,
		states: cache_item_with_handle.states,
		role: cache_item_with_handle.role,
		app: cache_item_with_handle.app,
		children: cache_item_with_handle.children,
		ifaces: cache_item_with_handle.ifaces,
		index: cache_item_with_handle.index,
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
        let (rh, wh) = evmap::with_hasher((), FxBuildHasher::default());

        Self { by_id_read: rh.factory(), by_id_write: Mutex::new(wh) }
    }
		/// add a single new item to the cache. Note that this will empty the bucket before inserting the `CacheItem` into the cache (this is so there is never two items with the same ID stored in the cache at the same time).
		pub async fn add(&self, cache_item: CacheItem) {
				let mut cache_writer = self.by_id_write.lock().await;
				cache_writer.empty(cache_item.object);
				cache_writer.insert(cache_item.object, cache_item);
				cache_writer.refresh();
		}
		/// remove a single cache item
		pub async fn remove(&self, id: i32) {
				let mut cache_writer = self.by_id_write.lock().await;
				cache_writer.empty(id);
				cache_writer.refresh();
		}
		/// get a single item from the cache (note that this copies some integers to a new struct)
		pub async fn get(&self, id: i32) -> Option<CacheItem> {
				let read_handle = self.by_id_read.handle();
				read_handle.get_one(&id).map(copy_into_cache_item)
		}
		/// get a many items from the cache; this only creates one read handle (note that this will copy all data you would like to access)
		pub async fn get_all(&self, ids: Vec<i32>) -> Vec<Option<CacheItem>> {
				let read_handle = self.by_id_read.handle();
				ids.iter()
					.map(|id| read_handle.get_one(id).map(copy_into_cache_item))
					.collect()
		}
		/// Bulk add many items to the cache; this only refreshes the cache after adding all items. Note that this will empty the bucket before inserting. Only one accessible should ever be associated with an id.
		pub async fn add_all(&self, cache_items: Vec<CacheItem>) {
				let mut cache_writer = self.by_id_write.lock().await;
				cache_items.into_iter()
					.for_each(|cache_item| {
						cache_writer.empty(cache_item.object);
						cache_writer.insert(cache_item.object, cache_item);
				});
				cache_writer.refresh();
		}
		/// Bulk remove all ids in the cache; this only refreshes the cache after removing all items.
		pub async fn remove_all(&self, ids: Vec<i32>) {
				let mut cache_writer = self.by_id_write.lock().await;
				ids.into_iter()
					.for_each(|id| {
						cache_writer.empty(id);
				});
				cache_writer.refresh();
		}
}
