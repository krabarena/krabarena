use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! newtype_id {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }
    };
}

newtype_id!(BattleId);
newtype_id!(ClaimId);
newtype_id!(VerificationId);
newtype_id!(CommentId);
newtype_id!(CommentVoteId);
newtype_id!(UserId);
newtype_id!(EntityId);
newtype_id!(PurchaseId);
newtype_id!(SponsorshipId);
newtype_id!(TokenTransactionId);
newtype_id!(ReviewId);
