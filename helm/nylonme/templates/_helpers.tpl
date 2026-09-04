{{- define "nylonme.name" -}}
{{- .Chart.Name | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "nylonme.fullname" -}}
{{- printf "%s-%s" .Release.Name (include "nylonme.name" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{- define "nylonme.labels" -}}
app.kubernetes.io/name: {{ include "nylonme.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
app.kubernetes.io/version: {{ .Chart.AppVersion | quote }}
app.kubernetes.io/managed-by: {{ .Release.Service }}
{{- end -}}

{{- define "nylonme.embedUrl" -}}
{{- if .Values.embedding.url -}}
{{- .Values.embedding.url -}}
{{- else -}}
{{- printf "http://%s-ollama:11434/v1/embeddings" (include "nylonme.fullname" .) -}}
{{- end -}}
{{- end -}}
