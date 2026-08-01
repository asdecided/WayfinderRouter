{{/* Expand the chart name. */}}
{{- define "wayfinder-router.name" -}}
{{- default .Chart.Name .Values.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Create a fully qualified app name. */}}
{{- define "wayfinder-router.fullname" -}}
{{- if .Values.fullnameOverride }}
{{- .Values.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default .Chart.Name .Values.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/* Common labels. */}}
{{- define "wayfinder-router.labels" -}}
helm.sh/chart: {{ include "wayfinder-router.chart" . }}
{{ include "wayfinder-router.selectorLabels" . }}
{{- if .Chart.AppVersion }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
{{- end }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end }}

{{/* Selector labels. */}}
{{- define "wayfinder-router.selectorLabels" -}}
app.kubernetes.io/name: {{ include "wayfinder-router.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/* Chart name and version. */}}
{{- define "wayfinder-router.chart" -}}
{{- printf "%s-%s" .Chart.Name .Chart.Version | replace "+" "_" | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/* Service account name. */}}
{{- define "wayfinder-router.serviceAccountName" -}}
{{- if .Values.serviceAccount.create }}
{{- default (include "wayfinder-router.fullname" .) .Values.serviceAccount.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.name }}
{{- end }}
{{- end }}

{{/* Redis service name. */}}
{{- define "wayfinder-router.redisName" -}}
{{- if .Values.redis.name }}
{{- .Values.redis.name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-redis" (include "wayfinder-router.fullname" .) | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}

{{/* Redis URL used by the generated configuration. */}}
{{- define "wayfinder-router.redisURL" -}}
{{- if .Values.redis.url }}
{{- .Values.redis.url }}
{{- else }}
{{- printf "redis://%s:%v" (include "wayfinder-router.redisName" .) .Values.redis.port }}
{{- end }}
{{- end }}

{{/* Container image, optionally pinned by digest. */}}
{{- define "wayfinder-router.image" -}}
{{- if .Values.image.digest }}
{{- printf "%s@%s" .Values.image.repository .Values.image.digest }}
{{- else }}
{{- printf "%s:%s" .Values.image.repository (default .Chart.AppVersion .Values.image.tag) }}
{{- end }}
{{- end }}
