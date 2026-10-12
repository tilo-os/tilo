# Published document readiness

A native application advertises and executes its published actions only after its
current original document histories have been applied. A successful channel join
alone does not establish that readiness.

The SDK checks the current primary route witness, verifies that every live
framework history owner is represented in the scoped route map, and consults the
existing transport document-publication gate for the primary and owner routes.
The same gate protects eager action publication, lazy directory queries and
resolution, and delivery of an already accepted invocation. Losing original
history after admission refuses the handler and retains an unknown outcome;
reconnecting and reconciling is required before retrying.

The collaboration pump uses the pure gate after its existing route refresh. Calls
outside that pump refresh first, so readiness cannot rely on an obsolete owner or
route. Legacy peer transports keep their existing explicit document-publication
contract. A disabled transport is not a readiness witness.

The socket-backed regression tests cover withheld initial publication, refused
early delivery, capture retirement, legacy compatibility, and two live record
owners. The admitted-delivery unit fixtures use the supported legacy peer seam;
they do not claim authenticated native history or caller admission.
