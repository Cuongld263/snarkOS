use crate::merkle_tree::MerklePath;
use snarkos_errors::algorithms::MerkleError;
use snarkos_models::algorithms::{MerkleParameters, CRH};
use snarkos_utilities::ToBytes;

pub struct MerkleTree<P: MerkleParameters> {
    root: Option<<P::H as CRH>::Output>,
    tree: Vec<<P::H as CRH>::Output>,
    leaves_hashed: Vec<<P::H as CRH>::Output>,
    padding_tree: Vec<(<P::H as CRH>::Output, <P::H as CRH>::Output)>, // For each level after a full tree has been built from the leaves, keeps both the roots the siblings that are used to get to the desired height.
    parameters: P,
}

impl<P: MerkleParameters> MerkleTree<P> {
    pub const HEIGHT: u8 = P::HEIGHT as u8;

    pub fn new<L: ToBytes>(parameters: P, leaves: &[L]) -> Result<Self, MerkleError> {
        let new_time = start_timer!(|| "MerkleTree::new");

        let hash_input_size_in_bytes = (P::H::INPUT_SIZE_BITS / 8) * 2;
        let last_level_size = leaves.len().next_power_of_two();
        let tree_size = 2 * last_level_size - 1;
        let tree_height = tree_height(tree_size);

        if tree_height > Self::HEIGHT as usize {
            return Err(MerkleError::InvalidTreeHeight(tree_height, Self::HEIGHT as usize));
        }

        // Initialize the Merkle tree.
        let mut tree = Vec::with_capacity(tree_size);
        let empty_hash = parameters.hash_empty()?;
        for _ in 0..tree_size {
            tree.push(empty_hash.clone());
        }

        // Compute the starting indices for each level of the tree.
        let mut index = 0;
        let mut level_indices = Vec::with_capacity(tree_height);
        for _ in 0..tree_height {
            level_indices.push(index);
            index = left_child(index);
        }

        // Compute and store the hash values for each leaf.
        let last_level_index = level_indices.pop().unwrap_or(0);
        let mut buffer = vec![0u8; hash_input_size_in_bytes];
        for (i, leaf) in leaves.iter().enumerate() {
            tree[last_level_index + i] = parameters.hash_leaf(leaf, &mut buffer)?;
        }

        // Compute the hash values for every node in the tree.
        let mut upper_bound = last_level_index;
        let mut buffer = vec![0u8; hash_input_size_in_bytes];
        level_indices.reverse();
        for &start_index in &level_indices {
            // Iterate over the current level.
            for current_index in start_index..upper_bound {
                let left_index = left_child(current_index);
                let right_index = right_child(current_index);

                // Compute Hash(left || right).
                tree[current_index] = parameters.hash_inner_node(&tree[left_index], &tree[right_index], &mut buffer)?;
            }
            upper_bound = start_index;
        }

        // Finished computing actual tree.
        // Now, we compute the dummy nodes until we hit our HEIGHT goal.
        let mut current_height = tree_height;
        let mut padding_tree = vec![];
        let mut current_hash = tree[0].clone();
        while current_height < Self::HEIGHT as usize {
            current_hash = parameters.hash_inner_node(&current_hash, &empty_hash, &mut buffer)?;

            // do not pad at the top-level of the tree
            if current_height < Self::HEIGHT as usize - 1 {
                padding_tree.push((current_hash.clone(), empty_hash.clone()));
            }
            current_height += 1;
        }
        let root_hash = current_hash;

        end_timer!(new_time);

        let leaves_hashed = tree[last_level_index..].to_vec();
        Ok(MerkleTree {
            tree,
            padding_tree,
            leaves_hashed,
            parameters,
            root: Some(root_hash),
        })
    }

    #[inline]
    pub fn root(&self) -> <P::H as CRH>::Output {
        self.root.clone().unwrap()
    }

    #[inline]
    pub fn leaves_hashed(&self) -> Vec<<P::H as CRH>::Output> {
        self.leaves_hashed.clone()
    }

    pub fn generate_proof<L: ToBytes>(&self, index: usize, leaf: &L) -> Result<MerklePath<P>, MerkleError> {
        let prove_time = start_timer!(|| "MerkleTree::generate_proof");
        let mut path = vec![];

        let hash_input_size_in_bytes = (P::H::INPUT_SIZE_BITS / 8) * 2;
        let mut buffer = vec![0u8; hash_input_size_in_bytes];

        let leaf_hash = self.parameters.hash_leaf(leaf, &mut buffer)?;

        let tree_height = tree_height(self.tree.len());
        let tree_index = convert_index_to_last_level(index, tree_height);

        // Check that the given index corresponds to the correct leaf.
        if leaf_hash != self.tree[tree_index] {
            return Err(MerkleError::IncorrectLeafIndex(tree_index));
        }

        // Iterate from the leaf up to the root, storing all intermediate hash values.
        let mut current_node = tree_index;
        while !is_root(current_node) {
            let sibling_node = sibling(current_node).unwrap();
            let (curr_hash, sibling_hash) = (self.tree[current_node].clone(), self.tree[sibling_node].clone());
            if is_left_child(current_node) {
                path.push((curr_hash, sibling_hash));
            } else {
                path.push((sibling_hash, curr_hash));
            }
            current_node = parent(current_node).unwrap();
        }

        // Store the root node. Set boolean as true for consistency with digest location.
        if path.len() >= Self::HEIGHT as usize {
            return Err(MerkleError::InvalidPathLength(path.len(), Self::HEIGHT as usize));
        }

        if path.len() != (Self::HEIGHT - 1) as usize {
            let empty_hash = self.parameters.hash_empty()?;
            path.push((self.tree[0].clone(), empty_hash));

            for &(ref hash, ref sibling_hash) in &self.padding_tree {
                path.push((hash.clone(), sibling_hash.clone()));
            }
        }
        end_timer!(prove_time);

        if path.len() != (Self::HEIGHT - 1) as usize {
            Err(MerkleError::IncorrectPathLength(path.len()))
        } else {
            Ok(MerklePath {
                parameters: self.parameters.clone(),
                path,
            })
        }
    }
}

impl<P: MerkleParameters> Default for MerkleTree<P> {
    fn default() -> Self {
        MerkleTree {
            tree: vec![],
            padding_tree: vec![],
            leaves_hashed: vec![],
            root: None,
            parameters: P::default(),
        }
    }
}

/// Returns the height of the tree, given the size of the tree.
#[inline]
fn tree_height(tree_size: usize) -> usize {
    // Returns the log2 value of the given number.
    fn log2(number: usize) -> usize {
        (number as f64).log2() as usize
    }

    log2(tree_size + 1)
}

/// Returns true iff the index represents the root.
#[inline]
fn is_root(index: usize) -> bool {
    index == 0
}

/// Returns the index of the left child, given an index.
#[inline]
fn left_child(index: usize) -> usize {
    2 * index + 1
}

/// Returns the index of the right child, given an index.
#[inline]
fn right_child(index: usize) -> usize {
    2 * index + 2
}

/// Returns the index of the sibling, given an index.
#[inline]
fn sibling(index: usize) -> Option<usize> {
    if index == 0 {
        None
    } else if is_left_child(index) {
        Some(index + 1)
    } else {
        Some(index - 1)
    }
}

/// Returns true iff the given index represents a left child.
#[inline]
fn is_left_child(index: usize) -> bool {
    index % 2 == 1
}

/// Returns the index of the parent, given an index.
#[inline]
fn parent(index: usize) -> Option<usize> {
    if index > 0 { Some((index - 1) >> 1) } else { None }
}

#[inline]
fn convert_index_to_last_level(index: usize, tree_height: usize) -> usize {
    index + (1 << (tree_height - 1)) - 1
}
