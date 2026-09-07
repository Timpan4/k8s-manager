mod argo;
mod cancellation;
mod cluster;
mod diagnostics;
mod discovery;
mod error;
mod events;
mod flux;
mod health;
mod helm;
mod incidents;
mod metrics;
mod namespace;
mod operations;
mod rbac;
mod resource;
mod sessions;
mod streams;
mod usage;

pub(crate) use argo::normalize_service_tunnel_endpoint;
pub use argo::{
    ArgoAppProjectDetails, ArgoAppProjectSummary, ArgoApplicationDetails, ArgoApplicationHistory,
    ArgoApplicationInspector, ArgoApplicationRef, ArgoApplicationSetDetails,
    ArgoApplicationSetSummary, ArgoApplicationSourceSummary, ArgoApplicationSummary,
    ArgoConnectedFallback, ArgoConnectionProfile, ArgoConnectionStatus, ArgoInspectionFailure,
    ArgoManagedResource, ArgoOperationConfirmation, ArgoOperationPreflight, ArgoOperationRequest,
    ArgoOperationResult, ArgoResourceComparison, ArgoServerCapability, ArgoServerEndpoint,
    ArgoServiceTunnelUnavailableReason,
};
pub use cancellation::{CancelBackendRequestsResult, CancelWorkspaceRequestsResult};
pub use cluster::ClusterContext;
pub use diagnostics::{BackendDiagnosticEvent, BackendDiagnosticField, BackendDiagnosticStatus};
pub use discovery::DiscoveredResourceKind;
pub(crate) use error::{kube_error_kind, WorkspaceRequestCancelled};
pub use error::{AppError, AppErrorKind};
pub use events::ResourceEventSummary;
pub use flux::{
    FluxDetectionSummary, FluxInventoryResource, FluxResourceDetails, FluxResourceKind,
    FluxResourceSummary,
};
pub use health::{
    argo_application_set_health_assessment, argo_health_assessment, evaluate_health,
    HealthAssessment, HealthAssessmentCompleteness, HealthAssessmentEvidence,
    HealthAssessmentInput, HealthAssessmentSource, HealthAssessmentState,
};
pub use helm::{
    HelmManifestResourceSummary, HelmManifestSummary, HelmReconciliationResource,
    HelmReconciliationStatus, HelmReconciliationTotals, HelmReleaseDetails,
    HelmReleaseReconciliation, HelmReleaseSummary, HelmValuesSummary,
};
pub use incidents::{
    IncidentCockpitItem, IncidentCockpitSummary, IncidentSeverity, IncidentSignalState,
    IncidentSignalSummary,
};
pub use metrics::{
    ResourceMetricSummary, ResourceMetricsAvailability, ResourceMetricsAvailabilityStatus,
    ResourceMetricsSummary,
};
pub use namespace::NamespaceSummary;
pub use operations::{
    ClusterOperationPreview, ClusterOperationResult, ClusterOperationTarget, DeleteResourceRequest,
    RolloutRestartRequest, ScaleWorkloadRequest,
};
pub use rbac::{
    RbacAccessReviewIdentity, RbacAccessReviewOutcome, RbacAccessReviewRequest,
    RbacAccessReviewResult, RbacAccessReviewTarget, RbacBindingSummary, RbacCoverageStatus,
    RbacFamily, RbacFamilyCoverage, RbacInspectionSummary, RbacNamespaceAccessSummary,
    RbacRequestMode, RbacRiskIndicator, RbacRiskLevel, RbacRoleSummary, RbacRuleSummary,
    RbacSubjectSummary, ServiceAccountSummary,
};
pub use resource::{
    DeploymentRevision, GitOpsOwnerSummary, KubernetesYamlLintDiagnostic, KubernetesYamlLintResult,
    KubernetesYamlLintSeverity, KubernetesYamlLintStatusNote, OwnerReferenceSummary,
    ResourceDetails, ResourceDetailsFull, ResourceHealth, ResourceListRequest, ResourceSummary,
    ResourceTopology, TopologyEdge, TopologyNode, TopologyRelation, YamlApplyPreview,
    YamlApplyRequest, YamlApplyResult, YamlApplyTarget, YamlEncoding, YamlViewMode,
};
pub use sessions::{
    LiveSessionCleanupRequest, LiveSessionCleanupResult, PodExecConfirmation,
    PodExecSessionMessage, PodExecSessionRequest, PodExecSessionSummary, PodExecTerminalSize,
    PortForwardRequest, PortForwardSessionSummary,
};
pub use streams::{
    AggregatedLogStreamRequest, LogLineSource, PodLogStreamRequest, StreamMessage,
    WatchResourceKey, WatchResourceKind, WatchResourceTarget,
};
pub use usage::{AppUsageMetrics, AppUsageMetricsBreakdown};
