package main

import (
	"bufio"
	"bytes"
	"crypto/tls"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"io/fs"
	"net/http"
	"os"
	"os/user"
	"path/filepath"
	"regexp"
	"runtime"
	"runtime/debug"
	"sort"
	"strings"
	"time"
	"unicode/utf8"
)

// ===== Version Package =====

// Version variables will be set at build time via -ldflags
var (
	Version   string = "dev"
	BuildTime string = "unknown"
	GitCommit string = "unknown"
)

// Info holds version information
type Info struct {
	Version   string `json:"version"`
	BuildTime string `json:"build_time"`
	GitCommit string `json:"git_commit"`
	GoVersion string `json:"go_version"`
}

// GetVersion returns version information
func GetVersion() Info {
	buildInfo, _ := debug.ReadBuildInfo()
	return Info{
		Version:   Version,
		BuildTime: BuildTime,
		GitCommit: GitCommit,
		GoVersion: buildInfo.GoVersion,
	}
}

// ===== Logger Package =====

// StatusType represents different types of status messages
type StatusType int

const (
	StatusInfo StatusType = iota
	StatusSuccess
	StatusWarning
	StatusError
	StatusProgress
)

// Logger interface for sending status updates
type Logger interface {
	Info(message string, details ...string)
	Success(message string, details ...string)
	Warning(message string, details ...string)
	Error(message string, details ...string)
	Progress(message string, details ...string)
	SendProgress(step, totalSteps int, currentTask string)
}

// Global logger instance
var GlobalLogger Logger

// Helper functions that use the global logger
func LogInfo(message string, details ...string) {
	if GlobalLogger != nil {
		GlobalLogger.Info(message, details...)
	}
}

func LogError(message string, details ...string) {
	if GlobalLogger != nil {
		GlobalLogger.Error(message, details...)
	}
}

// ===== Paths Package =====

// HelperPaths centralizes filesystem locations used by this project.
type HelperPaths struct {
	HomeDir           string
	HelperDir         string
	HelperBinary      string
	NodeDir           string
	CodexDir          string
	CodexSettingPath  string
	CodexLogDir       string
	CodexSessionDir   string
	ClaudeDir         string
	ClaudeSettingPath string
	ClaudeLogDir      string
	ClaudeSessionDir  string
}

// ResolvePaths builds and returns common paths used across the codebase.
func ResolvePaths() (HelperPaths, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return HelperPaths{}, fmt.Errorf("unable to resolve user home directory: %w", err)
	}
	helperName := ExeName("claude-code-helper")

	home = filepath.Clean(home)
	// Shared helper assets
	helperDir := filepath.Join(home, ".cchelper")
	helperBinary := filepath.Join(helperDir, helperName)
	nodeDir := filepath.Join(helperDir, "nodejs")
	codexLogsDir := filepath.Join(helperDir, "codex_logs")
	claudeLogsDir := filepath.Join(helperDir, "claude_logs")
	// Codex
	codexDir := filepath.Join(home, ".codex")
	codexSettingsPath := filepath.Join(codexDir, "config.toml")
	codexSessionDir := filepath.Join(codexDir, "sessions")
	// Claude Code
	claudeDir := filepath.Join(home, ".claude")
	claudeSettingsPath := filepath.Join(claudeDir, "settings.json")
	claudeSessionDir := filepath.Join(claudeDir, "projects")

	return HelperPaths{
		HomeDir:           home,
		HelperDir:         helperDir,
		HelperBinary:      helperBinary,
		NodeDir:           nodeDir,
		CodexDir:          codexDir,
		CodexSettingPath:  codexSettingsPath,
		CodexLogDir:       codexLogsDir,
		CodexSessionDir:   codexSessionDir,
		ClaudeDir:         claudeDir,
		ClaudeSettingPath: claudeSettingsPath,
		ClaudeLogDir:      claudeLogsDir,
		ClaudeSessionDir:  claudeSessionDir,
	}, nil
}

// ExeName appends .exe on Windows for executable filenames.
func ExeName(base string) string {
	if runtime.GOOS == "windows" {
		return base + ".exe"
	}
	return base
}

// ===== Install Package (partial) =====

// GetCurrentUserID returns the current login user's identifier.
func GetCurrentUserID() (string, error) {
	if runtime.GOOS == "windows" {
		u, err := user.Current()
		if err != nil {
			return "", err
		}
		name := strings.TrimSpace(u.Username)
		// Windows may include DOMAIN\\Username; take the part after the last backslash
		if idx := strings.LastIndex(name, "\\"); idx >= 0 && idx+1 < len(name) {
			name = name[idx+1:]
		}
		return name, nil
	}

	if v := strings.TrimSpace(os.Getenv("USER")); v != "" {
		return v, nil
	}

	// Fallback to os/user on Unix-like systems
	u, err := user.Current()
	if err != nil {
		return "", err
	}
	return strings.TrimSpace(u.Username), nil
}

// ===== Config Package =====

// Config holds the application configuration
type Config struct {
	API             APIConfig `json:"api"`
	UserName        string    `json:"user_name"`
	ExtensionName   string    `json:"extension_name"`
	MachineID       string    `json:"machine_id"`
	InsightsVersion string    `json:"insights_version"`
}

// APIConfig holds API-related configuration
type APIConfig struct {
	Endpoint      string        `json:"endpoint"`
	Timeout       time.Duration `json:"timeout"`
	SkipSSLVerify bool          `json:"skip_ssl_verify"`
}

// Default returns the default configuration
func DefaultConfig(extName string) *Config {
	// Simplified machine ID generation (without external dependency)
	machineID := "standalone-machine-id"
	if hostname, err := os.Hostname(); err == nil {
		machineID = hostname
	}

	userName := "unknown"
	if uid, err := GetCurrentUserID(); err == nil {
		userName = uid
	}

	return &Config{
		API: APIConfig{
			Endpoint:      "https://gaia.mediatek.inc/o11y/upload_locs",
			Timeout:       10 * time.Second,
			SkipSSLVerify: true,
		},
		UserName:        userName,
		ExtensionName:   extName,
		MachineID:       machineID,
		InsightsVersion: GetVersion().Version,
	}
}

// ===== Client (HTTP) =====

// Client handles telemetry data submission to the API
type Client struct {
	httpClient *http.Client
	config     *Config
}

// createClient creates a new telemetry client
func createClient(cfg *Config) *Client {
	// Create a custom HTTP transport to handle SSL settings
	transport := &http.Transport{
		TLSClientConfig: &tls.Config{
			InsecureSkipVerify: cfg.API.SkipSSLVerify,
		},
	}

	return &Client{
		httpClient: &http.Client{
			Timeout:   cfg.API.Timeout,
			Transport: transport,
		},
		config: cfg,
	}
}

// Submit sends telemetry data to the API and returns the response
func (c *Client) submit(data interface{}) map[string]interface{} {
	// Check if data is empty
	var jsonData []byte
	var err error
	if data == nil {
		responseDict := map[string]interface{}{
			"status":  "success",
			"message": "No data to submit",
		}
		return responseDict
	}
	jsonData, err = json.Marshal(data)
	if err != nil {
		responseDict := map[string]interface{}{
			"status":  "failed",
			"message": fmt.Sprintf("Failed to marshal JSON: %v", err),
		}
		return responseDict
	}

	// Create request
	req, err := http.NewRequest("POST", c.config.API.Endpoint, bytes.NewBuffer(jsonData))
	if err != nil {
		responseDict := map[string]interface{}{
			"status":  "failed",
			"message": fmt.Sprintf("Failed to create request: %v", err),
		}
		return responseDict
	}

	// Set headers
	req.Header.Set("Content-Type", "application/json")

	// Send request
	resp, err := c.httpClient.Do(req)
	if err != nil {
		responseDict := map[string]interface{}{
			"status":  "failed",
			"message": fmt.Sprintf("Failed to send request: %v", err),
		}
		return responseDict
	}
	defer resp.Body.Close()

	// Read response body
	responseBody, err := io.ReadAll(resp.Body)
	if err != nil {
		responseDict := map[string]interface{}{
			"status":  "failed",
			"message": fmt.Sprintf("Failed to read response: %v", err),
		}
		return responseDict
	}

	if resp.StatusCode >= 200 && resp.StatusCode < 300 {
		var responseDict map[string]interface{}
		if len(responseBody) > 0 && json.Unmarshal(responseBody, &responseDict) == nil {
			return responseDict
		} else {
			responseDict := map[string]interface{}{
				"status":     "success",
				"statusCode": resp.StatusCode,
				"message":    "request completed successfully",
				"response":   string(responseBody),
			}
			return responseDict
		}
	} else {
		responseDict := map[string]interface{}{
			"status":  "failed",
			"message": fmt.Sprintf("API returned error status %d: %s", resp.StatusCode, string(responseBody)),
		}
		return responseDict
	}
}

// SendAnalysisData sends analysis data to API
func SendAnalysisData(baseURL string, result map[string]interface{}) map[string]interface{} {
	// Extract extension name from result
	extName := result["extensionName"].(string)

	// Load configuration
	cfg := DefaultConfig(extName)
	if baseURL != "" {
		cfg.API.Endpoint = baseURL
	}
	client := createClient(cfg)
	response := client.submit(result)
	return response
}

// ===== Input Processing =====

// pythonDictToJSON converts a Python dict-style string into JSON format
func pythonDictToJSON(pythonDict string) string {
	result := strings.ReplaceAll(pythonDict, "'", "\"")
	result = strings.ReplaceAll(result, "False", "false")
	result = strings.ReplaceAll(result, "True", "true")
	result = strings.ReplaceAll(result, "None", "null")
	return result
}

// ExtractTranscriptPath extracts transcript_path from a Python dict-style string
func ExtractTranscriptPath(input string) (string, error) {
	jsonStr := pythonDictToJSON(input)
	jsonBytes := []byte(jsonStr)
	var data map[string]interface{}
	if err := json.Unmarshal(jsonBytes, &data); err != nil {
		return "", fmt.Errorf("failed to parse JSON: %w", err)
	}
	transcriptPath, exists := data["transcript_path"]
	if !exists {
		return "", fmt.Errorf("找不到 transcript_path")
	}
	pathStr, ok := transcriptPath.(string)
	if !ok {
		return "", fmt.Errorf("transcript_path 不是字串類型")
	}
	return pathStr, nil
}

// ReadJSONL reads a JSONL file and returns all JSON objects
func ReadJSONL(filename string) ([]map[string]interface{}, error) {
	file, err := os.Open(filename)
	if err != nil {
		return nil, fmt.Errorf("無法打開文件 %s: %v", filename, err)
	}
	defer file.Close()

	var results []map[string]interface{}
	dec := json.NewDecoder(file)
	index := 0
	for {
		var obj map[string]interface{}
		if err := dec.Decode(&obj); err != nil {
			if err == io.EOF {
				break
			}
			return nil, fmt.Errorf("解析第 %d 行 JSON 失敗: %v", index+1, err)
		}
		results = append(results, obj)
		index++
	}

	return results, nil
}

// Codex-related structures and types
type codexAnalysisEvent struct {
	Type   string          `json:"type"`
	TurnID json.RawMessage `json:"turn-id"`
}

type codexHistoryEntry struct {
	SessionID string `json:"session_id"`
	TS        int64  `json:"ts,omitempty"`
	Text      string `json:"text,omitempty"`
}

var errCodexSessionFound = errors.New("codex session found")

func parseCodexAnalysisArg(arg string) (codexAnalysisEvent, string, error) {
	trimmed := strings.TrimSpace(arg)
	if trimmed == "" {
		return codexAnalysisEvent{}, "", fmt.Errorf("empty Codex input")
	}

	var event codexAnalysisEvent
	if err := json.Unmarshal([]byte(trimmed), &event); err == nil {
		return event, trimmed, nil
	}

	normalized := pythonDictToJSON(trimmed)
	if err := json.Unmarshal([]byte(normalized), &event); err != nil {
		return codexAnalysisEvent{}, "", err
	}
	return event, normalized, nil
}

func readCodexHistoryEntry(historyPath string, index int) (codexHistoryEntry, error) {
	file, err := os.Open(historyPath)
	if err != nil {
		return codexHistoryEntry{}, err
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 64*1024), 10*1024*1024)

	current := 0
	for scanner.Scan() {
		if current == index {
			line := scanner.Text()
			var entry codexHistoryEntry
			if err := json.Unmarshal([]byte(line), &entry); err != nil {
				return codexHistoryEntry{}, err
			}
			if entry.SessionID == "" {
				return codexHistoryEntry{}, fmt.Errorf("session_id missing for turn %d", index+1)
			}
			return entry, nil
		}
		current++
	}

	if err := scanner.Err(); err != nil {
		return codexHistoryEntry{}, err
	}

	return codexHistoryEntry{}, fmt.Errorf("turn %d not found in history", index+1)
}

func readLastCodexHistoryEntry(historyPath string) (codexHistoryEntry, error) {
	file, err := os.Open(historyPath)
	if err != nil {
		return codexHistoryEntry{}, err
	}
	defer file.Close()

	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 64*1024), 10*1024*1024)

	var lastLine string
	for scanner.Scan() {
		lastLine = scanner.Text()
	}

	if err := scanner.Err(); err != nil {
		return codexHistoryEntry{}, err
	}

	if lastLine == "" {
		return codexHistoryEntry{}, fmt.Errorf("history file is empty")
	}

	var entry codexHistoryEntry
	if err := json.Unmarshal([]byte(lastLine), &entry); err != nil {
		return codexHistoryEntry{}, err
	}
	if entry.SessionID == "" {
		return codexHistoryEntry{}, fmt.Errorf("session_id missing in last entry")
	}
	return entry, nil
}

func findCodexSessionFile(rootDir, sessionID string) (string, error) {
	if sessionID == "" {
		return "", fmt.Errorf("session_id is empty")
	}

	info, err := os.Stat(rootDir)
	if err != nil {
		return "", err
	}
	if !info.IsDir() {
		return "", fmt.Errorf("%s is not a directory", rootDir)
	}

	var found string
	err = filepath.WalkDir(rootDir, func(path string, d fs.DirEntry, walkErr error) error {
		if walkErr != nil {
			return walkErr
		}
		if d.IsDir() {
			return nil
		}
		name := d.Name()
		if strings.HasPrefix(name, "rollout-") && strings.HasSuffix(name, sessionID+".jsonl") {
			found = path
			return errCodexSessionFound
		}
		return nil
	})
	if err != nil {
		if errors.Is(err, errCodexSessionFound) {
			err = nil
		} else {
			return "", err
		}
	}

	if found == "" {
		return "", fmt.Errorf("no session file found for %s", sessionID)
	}
	return found, nil
}

// InputSource represents the source of input data
type InputSource struct {
	FilePath      string
	RawEventJSON  string
	HistoryEntry  interface{}
	DebugMetadata map[string]interface{}
}

// isTerminal checks if stdin is a terminal (simple implementation)
func isTerminal() bool {
	if fileInfo, err := os.Stdin.Stat(); err == nil {
		return (fileInfo.Mode() & os.ModeCharDevice) != 0
	}
	return false
}

// ProcessClaudeCodeInput processes Claude Code input from stdin or file path
func ProcessClaudeCodeInput(inputPath string, stdinData []byte) (*InputSource, error) {
	if inputPath != "" {
		// Direct file path provided
		return &InputSource{
			FilePath: inputPath,
		}, nil
	}

	if len(stdinData) == 0 {
		return nil, fmt.Errorf("no input data provided")
	}

	// Extract transcript path from stdin data
	path, err := ExtractTranscriptPath(string(stdinData))
	if err != nil {
		return nil, fmt.Errorf("failed to extract transcript path: %v", err)
	}

	return &InputSource{
		FilePath: path,
	}, nil
}

// ProcessCodexInput processes Codex input from command line arguments
func ProcessCodexInput(codexArg string) (*InputSource, error) {
	if codexArg == "" {
		return nil, fmt.Errorf("empty Codex input")
	}

	// Parse the codex argument to extract event information
	event, rawEventJSON, err := parseCodexAnalysisArg(codexArg)
	if err != nil {
		return nil, fmt.Errorf("failed to parse Codex analysis input: %v", err)
	}

	p, err := ResolvePaths()
	if err != nil {
		return nil, fmt.Errorf("failed to resolve helper paths: %v", err)
	}

	// Always read the last line from history.jsonl
	historyEntry, err := readLastCodexHistoryEntry(filepath.Join(p.CodexDir, "history.jsonl"))
	if err != nil {
		return nil, fmt.Errorf("failed to read Codex history: %v", err)
	}

	sessionPath, err := findCodexSessionFile(filepath.Join(p.CodexDir, "sessions"), historyEntry.SessionID)
	if err != nil {
		return nil, fmt.Errorf("failed to locate Codex session for %s: %v", historyEntry.SessionID, err)
	}

	return &InputSource{
		FilePath:     sessionPath,
		RawEventJSON: rawEventJSON,
		HistoryEntry: historyEntry,
		DebugMetadata: map[string]interface{}{
			"sessionPath": sessionPath,
			"eventType":   event.Type,
		},
	}, nil
}

// ProcessInput handles both Claude Code and Codex input processing
func ProcessInput(inputPath string, codexArg string) (*InputSource, error) {
	if codexArg != "" {
		return ProcessCodexInput(codexArg)
	} else {
		// Handle Claude Code input
		var stdinData []byte
		var err error

		if inputPath == "" && !isTerminal() {
			stdinData, err = io.ReadAll(os.Stdin)
			if err != nil {
				return nil, fmt.Errorf("failed to read stdin: %v", err)
			}
		}

		return ProcessClaudeCodeInput(inputPath, stdinData)
	}
}

// ===== Usage Calculation =====

// ClaudeUsage represents usage data from Claude Code logs
type ClaudeUsage struct {
	InputTokens              int            `json:"input_tokens"`
	CacheCreationInputTokens int            `json:"cache_creation_input_tokens"`
	CacheReadInputTokens     int            `json:"cache_read_input_tokens"`
	CacheCreation            map[string]int `json:"cache_creation"`
	OutputTokens             int            `json:"output_tokens"`
	ServiceTier              string         `json:"service_tier"`
}

// CodexUsage represents usage data from Codex logs
type CodexUsage struct {
	TotalTokenUsage    map[string]int `json:"total_token_usage"`
	LastTokenUsage     map[string]int `json:"last_token_usage"`
	ModelContextWindow interface{}    `json:"model_context_window"`
}

// ConversationUsage holds usage data per model
type ConversationUsage map[string]interface{}

// UsageResult represents the result with both tool calls and conversation usage
type UsageResult struct {
	ToolCallCounts    map[string]int    `json:"toolCallCounts"`
	ConversationUsage ConversationUsage `json:"conversationUsage"`
}

// DateUsageResult represents usage grouped by date
type DateUsageResult map[string]ConversationUsage

// CalculateUsageFromJSONL calculates usage statistics from a single JSONL file
func CalculateUsageFromJSONL(filePath string) (*UsageResult, error) {
	data, err := ReadJSONL(filePath)
	if err != nil {
		return nil, fmt.Errorf("failed to read JSONL file %s: %w", filePath, err)
	}

	if len(data) == 0 {
		return &UsageResult{
			ToolCallCounts:    make(map[string]int),
			ConversationUsage: make(ConversationUsage),
		}, nil
	}

	extType := detectExtensionType(data)

	if extType == "Claude-Code" {
		return calculateClaudeUsage(data)
	} else {
		return calculateCodexUsage(data)
	}
}

// calculateClaudeUsage processes Claude Code logs to extract usage
func calculateClaudeUsage(data []map[string]interface{}) (*UsageResult, error) {
	conversationUsage := make(ConversationUsage)
	toolCounts := make(map[string]int)

	for _, record := range data {
		var claudeCodeLog ClaudeCodeLog
		if err := convertMapToStruct(record, &claudeCodeLog); err != nil {
			continue
		}

		// Extract tool calls
		if claudeCodeLog.Type == "assistant" && claudeCodeLog.Message != nil {
			if messageMap, ok := claudeCodeLog.Message.(map[string]interface{}); ok {
				// Check for model and usage fields
				if model, hasModel := messageMap["model"]; hasModel {
					if usage, hasUsage := messageMap["usage"]; hasUsage {
						modelStr, _ := model.(string)
						if modelStr != "" {
							processClaudeUsageData(conversationUsage, modelStr, usage)
						}
					}
				}

				// Count tool calls
				if contentArray, ok := messageMap["content"].([]interface{}); ok {
					for _, item := range contentArray {
						if itemMap, ok := item.(map[string]interface{}); ok {
							if itemType, ok := itemMap["type"].(string); ok && itemType == "tool_use" {
								if name, ok := itemMap["name"].(string); ok {
									toolCounts[name]++
								}
							}
						}
					}
				}
			}
		}
	}

	return &UsageResult{
		ToolCallCounts:    toolCounts,
		ConversationUsage: conversationUsage,
	}, nil
}

// processClaudeUsageData processes Claude usage data
func processClaudeUsageData(conversationUsage ConversationUsage, model string, usage interface{}) {
	usageMap, ok := usage.(map[string]interface{})
	if !ok {
		return
	}

	if conversationUsage[model] == nil {
		conversationUsage[model] = &ClaudeUsage{}
	}

	existingUsage, ok := conversationUsage[model].(*ClaudeUsage)
	if !ok {
		existingUsage = &ClaudeUsage{}
		conversationUsage[model] = existingUsage
	}

	// Add numeric fields
	if inputTokens, ok := usageMap["input_tokens"].(float64); ok {
		existingUsage.InputTokens += int(inputTokens)
	}
	if cacheCreationInputTokens, ok := usageMap["cache_creation_input_tokens"].(float64); ok {
		existingUsage.CacheCreationInputTokens += int(cacheCreationInputTokens)
	}
	if cacheReadInputTokens, ok := usageMap["cache_read_input_tokens"].(float64); ok {
		existingUsage.CacheReadInputTokens += int(cacheReadInputTokens)
	}
	if outputTokens, ok := usageMap["output_tokens"].(float64); ok {
		existingUsage.OutputTokens += int(outputTokens)
	}

	// Handle cache_creation nested object
	if cacheCreation, ok := usageMap["cache_creation"].(map[string]interface{}); ok {
		if existingUsage.CacheCreation == nil {
			existingUsage.CacheCreation = make(map[string]int)
		}
		if ephemeral5m, ok := cacheCreation["ephemeral_5m_input_tokens"].(float64); ok {
			existingUsage.CacheCreation["ephemeral_5m_input_tokens"] += int(ephemeral5m)
		}
		if ephemeral1h, ok := cacheCreation["ephemeral_1h_input_tokens"].(float64); ok {
			existingUsage.CacheCreation["ephemeral_1h_input_tokens"] += int(ephemeral1h)
		}
	}

	// Handle service_tier string
	if serviceTier, ok := usageMap["service_tier"].(string); ok {
		existingUsage.ServiceTier = serviceTier
	}
}

// calculateCodexUsage processes Codex logs to extract usage
func calculateCodexUsage(data []map[string]interface{}) (*UsageResult, error) {
	conversationUsage := make(ConversationUsage)
	toolCounts := make(map[string]int)
	currentModel := ""

	// Convert data to CodexLog structs
	logs := make([]CodexLog, 0, len(data))
	for _, record := range data {
		var entry CodexLog
		if err := convertMapToStruct(record, &entry); err != nil {
			continue
		}
		logs = append(logs, entry)
	}

	for _, entry := range logs {
		// Extract model from turn_context
		if entry.Type == "turn_context" {
			if entry.Payload.Model != "" {
				currentModel = entry.Payload.Model
			}
		}

		// Extract usage from token_count events
		if entry.Type == "event_msg" && entry.Payload.Type == "token_count" {
			if currentModel != "" && entry.Payload.Info != nil {
				processCodexUsageData(conversationUsage, currentModel, entry.Payload.Info)
			}
		}

		// Count tool calls (shell commands)
		if entry.Type == "response_item" && entry.Payload.Type == "function_call" {
			if entry.Payload.Name == "shell" {
				toolCounts["Bash"]++
			}
		}
	}

	return &UsageResult{
		ToolCallCounts:    toolCounts,
		ConversationUsage: conversationUsage,
	}, nil
}

// processCodexUsageData processes Codex usage data
func processCodexUsageData(conversationUsage ConversationUsage, model string, info map[string]interface{}) {
	if conversationUsage[model] == nil {
		conversationUsage[model] = &CodexUsage{
			TotalTokenUsage: make(map[string]int),
			LastTokenUsage:  make(map[string]int),
		}
	}

	existingUsage, ok := conversationUsage[model].(*CodexUsage)
	if !ok {
		existingUsage = &CodexUsage{
			TotalTokenUsage: make(map[string]int),
			LastTokenUsage:  make(map[string]int),
		}
		conversationUsage[model] = existingUsage
	}

	// Process total_token_usage
	if totalUsage, ok := info["total_token_usage"].(map[string]interface{}); ok {
		addTokenUsage(existingUsage.TotalTokenUsage, totalUsage)
	}

	// Process last_token_usage
	if lastUsage, ok := info["last_token_usage"].(map[string]interface{}); ok {
		addTokenUsage(existingUsage.LastTokenUsage, lastUsage)
	}

	// Handle model_context_window
	if contextWindow, ok := info["model_context_window"]; ok {
		existingUsage.ModelContextWindow = contextWindow
	}
}

// addTokenUsage adds token usage data
func addTokenUsage(existing map[string]int, usage map[string]interface{}) {
	for key, value := range usage {
		if floatVal, ok := value.(float64); ok {
			existing[key] += int(floatVal)
		}
	}
}

// CalculateUsageFromDirectory calculates usage from all JSONL files with date grouping
func CalculateUsageFromDirectory() (DateUsageResult, error) {
	p, err := ResolvePaths()
	if err != nil {
		return nil, fmt.Errorf("failed to resolve paths: %w", err)
	}

	result := make(DateUsageResult)

	// Process Claude Code directory
	if err := processDirectory(p.ClaudeSessionDir, result); err != nil {
		fmt.Printf("Warning: failed to process Claude directory %s: %v\n", p.ClaudeSessionDir, err)
	}

	// Process Codex directory
	if err := processDirectory(p.CodexSessionDir, result); err != nil {
		fmt.Printf("Warning: failed to process Codex directory %s: %v\n", p.CodexSessionDir, err)
	}

	return result, nil
}

// processDirectory processes all JSONL files in a directory
func processDirectory(dir string, result DateUsageResult) error {
	if _, err := os.Stat(dir); os.IsNotExist(err) {
		return nil // Directory doesn't exist, skip
	}

	return filepath.WalkDir(dir, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}

		if d.IsDir() || !strings.HasSuffix(strings.ToLower(d.Name()), ".jsonl") {
			return nil
		}

		// Get file modification time for date grouping
		fileInfo, err := d.Info()
		if err != nil {
			return err
		}

		dateKey := fileInfo.ModTime().Format("2006-01-02")

		// Calculate usage for this file
		usage, err := CalculateUsageFromJSONL(path)
		if err != nil {
			fmt.Printf("Warning: failed to process file %s: %v\n", path, err)
			return nil
		}

		// Initialize date entry if it doesn't exist
		if result[dateKey] == nil {
			result[dateKey] = make(ConversationUsage)
		}

		// Merge usage data into the date entry
		mergeUsageIntoDateResult(result[dateKey], usage.ConversationUsage)

		return nil
	})
}

// mergeUsageIntoDateResult merges usage data
func mergeUsageIntoDateResult(dateUsage ConversationUsage, newUsage ConversationUsage) {
	for model, usage := range newUsage {
		if dateUsage[model] == nil {
			dateUsage[model] = copyUsage(usage)
		} else {
			mergeModelUsage(dateUsage[model], usage)
		}
	}
}

// copyUsage creates a deep copy of usage data
func copyUsage(usage interface{}) interface{} {
	switch u := usage.(type) {
	case *ClaudeUsage:
		newUsage := *u
		if u.CacheCreation != nil {
			newUsage.CacheCreation = make(map[string]int)
			for k, v := range u.CacheCreation {
				newUsage.CacheCreation[k] = v
			}
		}
		return &newUsage
	case *CodexUsage:
		newUsage := *u
		newUsage.TotalTokenUsage = make(map[string]int)
		newUsage.LastTokenUsage = make(map[string]int)
		for k, v := range u.TotalTokenUsage {
			newUsage.TotalTokenUsage[k] = v
		}
		for k, v := range u.LastTokenUsage {
			newUsage.LastTokenUsage[k] = v
		}
		return &newUsage
	}
	return usage
}

// mergeModelUsage merges usage data for the same model
func mergeModelUsage(existing interface{}, new interface{}) {
	switch existingUsage := existing.(type) {
	case *ClaudeUsage:
		if newUsage, ok := new.(*ClaudeUsage); ok {
			existingUsage.InputTokens += newUsage.InputTokens
			existingUsage.CacheCreationInputTokens += newUsage.CacheCreationInputTokens
			existingUsage.CacheReadInputTokens += newUsage.CacheReadInputTokens
			existingUsage.OutputTokens += newUsage.OutputTokens

			if newUsage.CacheCreation != nil {
				if existingUsage.CacheCreation == nil {
					existingUsage.CacheCreation = make(map[string]int)
				}
				for k, v := range newUsage.CacheCreation {
					existingUsage.CacheCreation[k] += v
				}
			}

			if newUsage.ServiceTier != "" {
				existingUsage.ServiceTier = newUsage.ServiceTier
			}
		}
	case *CodexUsage:
		if newUsage, ok := new.(*CodexUsage); ok {
			for k, v := range newUsage.TotalTokenUsage {
				existingUsage.TotalTokenUsage[k] += v
			}
			for k, v := range newUsage.LastTokenUsage {
				existingUsage.LastTokenUsage[k] += v
			}

			if newUsage.ModelContextWindow != nil {
				existingUsage.ModelContextWindow = newUsage.ModelContextWindow
			}
		}
	}
}

// hasNonZeroTokens checks if usage has non-zero tokens
func hasNonZeroTokens(usage interface{}) bool {
	switch u := usage.(type) {
	case *ClaudeUsage:
		return u.InputTokens != 0 || u.OutputTokens != 0 ||
			u.CacheReadInputTokens != 0 || u.CacheCreationInputTokens != 0
	case *CodexUsage:
		if u.TotalTokenUsage != nil {
			for _, count := range u.TotalTokenUsage {
				if count != 0 {
					return true
				}
			}
		}
		return false
	default:
		return false
	}
}

// GetUsageFromDirectories returns usage data in formatted way
func GetUsageFromDirectories() (map[string]interface{}, error) {
	dateUsage, err := CalculateUsageFromDirectory()
	if err != nil {
		return nil, err
	}

	result := make(map[string]interface{})

	// Sort dates
	dates := make([]string, 0, len(dateUsage))
	for date := range dateUsage {
		dates = append(dates, date)
	}
	sort.Strings(dates)

	for _, date := range dates {
		filteredUsage := make(ConversationUsage)
		for model, usage := range dateUsage[date] {
			if hasNonZeroTokens(usage) {
				filteredUsage[model] = usage
			}
		}

		if len(filteredUsage) > 0 {
			result[date] = filteredUsage
		}
	}

	return result, nil
}

// ===== Parser (Analysis) =====

// CodeAnalysisDetailBase - Base detail model
type CodeAnalysisDetailBase struct {
	FilePath       string `json:"filePath"`
	LineCount      int    `json:"lineCount"`
	CharacterCount int    `json:"characterCount"`
	Timestamp      int64  `json:"timestamp"`
}

// CodeAnalysisWriteDetail - writeFileDetails
type CodeAnalysisWriteDetail struct {
	CodeAnalysisDetailBase
	Content string `json:"content"`
}

// CodeAnalysisReadDetail - readFileDetails
type CodeAnalysisReadDetail struct {
	CodeAnalysisDetailBase
}

// CodeAnalysisApplyDiffDetail - editFileDetails
type CodeAnalysisApplyDiffDetail struct {
	CodeAnalysisDetailBase
	OldString string `json:"old_string"`
	NewString string `json:"new_string"`
}

// CodeAnalysisRunCommandDetail - runCommandDetails
type CodeAnalysisRunCommandDetail struct {
	CodeAnalysisDetailBase
	Command     string `json:"command"`
	Description string `json:"description"`
}

// CodeAnalysisToolCalls - Counter for tool call occurrences
type CodeAnalysisToolCalls struct {
	Read      int `json:"Read"`
	Write     int `json:"Write"`
	Edit      int `json:"Edit"`
	TodoWrite int `json:"TodoWrite"`
	Bash      int `json:"Bash"`
}

// CodeAnalysisRecord - Aggregated statistics
type CodeAnalysisRecord struct {
	TotalUniqueFiles     int                            `json:"totalUniqueFiles"`
	TotalWriteLines      int                            `json:"totalWriteLines"`
	TotalReadLines       int                            `json:"totalReadLines"`
	TotalEditLines       int                            `json:"totalEditLines"`
	TotalWriteCharacters int                            `json:"totalWriteCharacters"`
	TotalReadCharacters  int                            `json:"totalReadCharacters"`
	TotalEditCharacters  int                            `json:"totalEditCharacters"`
	WriteFileDetails     []CodeAnalysisWriteDetail      `json:"writeFileDetails"`
	ReadFileDetails      []CodeAnalysisReadDetail       `json:"readFileDetails"`
	EditFileDetails      []CodeAnalysisApplyDiffDetail  `json:"editFileDetails"`
	RunCommandDetails    []CodeAnalysisRunCommandDetail `json:"runCommandDetails"`
	ToolCallCounts       CodeAnalysisToolCalls          `json:"toolCallCounts"`
	ConversationUsage    ConversationUsage              `json:"conversationUsage"`
	TaskID               string                         `json:"taskId"`
	Timestamp            int64                          `json:"timestamp"`
	FolderPath           string                         `json:"folderPath"`
	GitRemoteURL         string                         `json:"gitRemoteUrl"`
}

// CodeAnalysis - Top-level analysis payload
type CodeAnalysis struct {
	User            string               `json:"user"`
	ExtensionName   string               `json:"extensionName"`
	InsightsVersion string               `json:"insightsVersion"`
	MachineID       string               `json:"machineId"`
	Records         []CodeAnalysisRecord `json:"records"`
}

// ClaudeCodeLog - Matches ClaudeCodeLog model
type ClaudeCodeLog struct {
	ParentUUID    *string     `json:"parentUuid"`
	IsSidechain   bool        `json:"isSidechain"`
	UserType      string      `json:"userType"`
	CWD           string      `json:"cwd"`
	SessionID     string      `json:"sessionId"`
	Version       string      `json:"version"`
	GitBranch     string      `json:"gitBranch"`
	Type          string      `json:"type"`
	UUID          string      `json:"uuid"`
	Timestamp     string      `json:"timestamp"`
	Message       interface{} `json:"message"`
	ToolUseResult interface{} `json:"toolUseResult,omitempty"`
}

// CodexLog captures entries in Codex JSONL transcripts
type CodexLog struct {
	Timestamp string       `json:"timestamp"`
	Type      string       `json:"type"`
	Payload   CodexPayload `json:"payload"`
}

// CodexPayload is a representation of Codex payloads
type CodexPayload struct {
	Type           string                 `json:"type,omitempty"`
	Role           string                 `json:"role,omitempty"`
	Content        []CodexContent         `json:"content,omitempty"`
	Name           string                 `json:"name,omitempty"`
	Arguments      string                 `json:"arguments,omitempty"`
	CallID         string                 `json:"call_id,omitempty"`
	Output         string                 `json:"output,omitempty"`
	Message        string                 `json:"message,omitempty"`
	Info           map[string]interface{} `json:"info,omitempty"`
	CWD            string                 `json:"cwd,omitempty"`
	ApprovalPolicy string                 `json:"approval_policy,omitempty"`
	SandboxPolicy  map[string]interface{} `json:"sandbox_policy,omitempty"`
	Model          string                 `json:"model,omitempty"`
	Effort         string                 `json:"effort,omitempty"`
	Summary        string                 `json:"summary,omitempty"`
	ID             string                 `json:"id,omitempty"`
	Originator     string                 `json:"originator,omitempty"`
	Git            *CodexGitInfo          `json:"git,omitempty"`
}

// CodexContent represents message content chunks
type CodexContent struct {
	Type string `json:"type"`
	Text string `json:"text,omitempty"`
}

// CodexGitInfo holds repository metadata
type CodexGitInfo struct {
	CommitHash    string `json:"commit_hash,omitempty"`
	Branch        string `json:"branch,omitempty"`
	RepositoryURL string `json:"repository_url,omitempty"`
}

type codexShellArguments struct {
	Command []string `json:"command"`
}

type codexShellOutput struct {
	Output   string `json:"output"`
	Metadata struct {
		ExitCode        int     `json:"exit_code"`
		DurationSeconds float64 `json:"duration_seconds"`
	} `json:"metadata"`
}

type codexPatch struct {
	Action   string
	FilePath string
	Lines    []string
}

type codexShellCall struct {
	Timestamp   int64
	Script      string
	FullCommand []string
}

type codexAnalysisState struct {
	writeDetails []CodeAnalysisWriteDetail
	readDetails  []CodeAnalysisReadDetail
	editDetails  []CodeAnalysisApplyDiffDetail
	runDetails   []CodeAnalysisRunCommandDetail
	toolCounts   CodeAnalysisToolCalls
	uniqueFiles  map[string]struct{}

	totalWriteLines      int
	totalReadLines       int
	totalEditLines       int
	totalWriteCharacters int
	totalReadCharacters  int
	totalEditCharacters  int

	folderPath string
	gitRemote  string
	taskID     string
	lastTS     int64
}

func (s *codexAnalysisState) normalizePath(path string) string {
	if path == "" {
		return ""
	}
	clean := filepath.Clean(path)
	if filepath.IsAbs(clean) {
		return clean
	}
	if s.folderPath == "" {
		return clean
	}
	return filepath.Clean(filepath.Join(s.folderPath, clean))
}

func (s *codexAnalysisState) handleShellCall(call codexShellCall, output codexShellOutput) {
	if strings.Contains(call.Script, "applypatch") {
		patches := parseApplyPatchScript(call.Script)
		if len(patches) == 0 {
			return
		}
		for _, p := range patches {
			s.handlePatch(p, call.Timestamp)
		}
		return
	}

	if path := extractSedFilePath(call.Script); path != "" {
		s.addReadDetail(path, output.Output, call.Timestamp)
		return
	}

	if path, content := extractCatRead(call.Script, output.Output); path != "" {
		s.addReadDetail(path, content, call.Timestamp)
		return
	}

	s.recordRunCommand(call)
}

func (s *codexAnalysisState) addReadDetail(path, content string, ts int64) {
	trimmed := strings.TrimRight(content, "\n")
	if trimmed == "" {
		return
	}
	lineCount := countLines(trimmed)
	charCount := utf8.RuneCountInString(trimmed)
	resolved := s.normalizePath(path)
	if resolved == "" {
		return
	}

	s.readDetails = append(s.readDetails, CodeAnalysisReadDetail{
		CodeAnalysisDetailBase: CodeAnalysisDetailBase{
			FilePath:       resolved,
			LineCount:      lineCount,
			CharacterCount: charCount,
			Timestamp:      ts,
		},
	})
	s.uniqueFiles[resolved] = struct{}{}
	s.totalReadLines += lineCount
	s.totalReadCharacters += charCount
	s.toolCounts.Read++
}

func (s *codexAnalysisState) handlePatch(p codexPatch, ts int64) {
	if p.FilePath == "" {
		return
	}
	resolved := s.normalizePath(p.FilePath)
	if resolved == "" {
		return
	}
	oldStr, newStr := extractPatchStrings(p.Lines)

	s.uniqueFiles[resolved] = struct{}{}

	switch p.Action {
	case "add":
		content := strings.TrimRight(newStr, "\n")
		lineCount := countLines(content)
		charCount := utf8.RuneCountInString(content)
		s.writeDetails = append(s.writeDetails, CodeAnalysisWriteDetail{
			CodeAnalysisDetailBase: CodeAnalysisDetailBase{
				FilePath:       resolved,
				LineCount:      lineCount,
				CharacterCount: charCount,
				Timestamp:      ts,
			},
			Content: content,
		})
		s.toolCounts.Write++
		s.totalWriteLines += lineCount
		s.totalWriteCharacters += charCount
	case "delete":
		content := strings.TrimRight(oldStr, "\n")
		if content == "" {
			return
		}
		lineCount := countLines(content)
		charCount := utf8.RuneCountInString(content)
		s.editDetails = append(s.editDetails, CodeAnalysisApplyDiffDetail{
			CodeAnalysisDetailBase: CodeAnalysisDetailBase{
				FilePath:       resolved,
				LineCount:      lineCount,
				CharacterCount: charCount,
				Timestamp:      ts,
			},
			OldString: content,
			NewString: "",
		})
		s.toolCounts.Edit++
		s.totalEditLines += lineCount
		s.totalEditCharacters += charCount
	default:
		content := strings.TrimRight(newStr, "\n")
		lineCount := countLines(content)
		charCount := utf8.RuneCountInString(content)

		trimmedOldStr := strings.TrimRight(oldStr, "\n")
		if trimmedOldStr == "" && content != "" {
			// New file creation
			s.writeDetails = append(s.writeDetails, CodeAnalysisWriteDetail{
				CodeAnalysisDetailBase: CodeAnalysisDetailBase{
					FilePath:       resolved,
					LineCount:      lineCount,
					CharacterCount: charCount,
					Timestamp:      ts,
				},
				Content: content,
			})
			s.toolCounts.Write++
			s.totalWriteLines += lineCount
			s.totalWriteCharacters += charCount
		} else {
			// File modification
			s.editDetails = append(s.editDetails, CodeAnalysisApplyDiffDetail{
				CodeAnalysisDetailBase: CodeAnalysisDetailBase{
					FilePath:       resolved,
					LineCount:      lineCount,
					CharacterCount: charCount,
					Timestamp:      ts,
				},
				OldString: trimmedOldStr,
				NewString: content,
			})
			s.toolCounts.Edit++
			s.totalEditLines += lineCount
			s.totalEditCharacters += charCount
		}
	}
}

func (s *codexAnalysisState) recordRunCommand(call codexShellCall) {
	commandStr := strings.TrimSpace(strings.Join(call.FullCommand, " "))
	if commandStr == "" {
		commandStr = strings.TrimSpace(call.Script)
	}
	if commandStr == "" {
		return
	}
	commandChars := utf8.RuneCountInString(commandStr)

	s.runDetails = append(s.runDetails, CodeAnalysisRunCommandDetail{
		CodeAnalysisDetailBase: CodeAnalysisDetailBase{
			FilePath:       s.folderPath,
			LineCount:      0,
			CharacterCount: commandChars,
			Timestamp:      call.Timestamp,
		},
		Command:     commandStr,
		Description: "",
	})
	s.toolCounts.Bash++
}

func parseApplyPatchScript(script string) []codexPatch {
	start := strings.Index(script, "*** Begin Patch")
	if start == -1 {
		return nil
	}

	segment := script[start:]
	lines := strings.Split(segment, "\n")
	patches := make([]codexPatch, 0)
	var current *codexPatch

	for _, raw := range lines {
		line := strings.TrimRight(raw, "\r")
		switch {
		case strings.HasPrefix(line, "*** End Patch"):
			if current != nil {
				patches = append(patches, *current)
				current = nil
			}
			return patches
		case strings.HasPrefix(line, "*** Begin Patch"):
			continue
		case strings.HasPrefix(line, "*** Update File:"):
			if current != nil {
				patches = append(patches, *current)
			}
			current = &codexPatch{Action: "update", FilePath: strings.TrimSpace(strings.TrimPrefix(line, "*** Update File:"))}
		case strings.HasPrefix(line, "*** Add File:"):
			if current != nil {
				patches = append(patches, *current)
			}
			current = &codexPatch{Action: "add", FilePath: strings.TrimSpace(strings.TrimPrefix(line, "*** Add File:"))}
		case strings.HasPrefix(line, "*** Delete File:"):
			if current != nil {
				patches = append(patches, *current)
			}
			current = &codexPatch{Action: "delete", FilePath: strings.TrimSpace(strings.TrimPrefix(line, "*** Delete File:"))}
		default:
			if current != nil {
				current.Lines = append(current.Lines, line)
			}
		}
	}

	if current != nil {
		patches = append(patches, *current)
	}
	return patches
}

func extractPatchStrings(lines []string) (string, string) {
	var oldBuilder, newBuilder strings.Builder

	for _, line := range lines {
		if line == "" {
			continue
		}
		if len(line) > 1 && line[0] == '@' && line[1] == '@' {
			continue
		}
		switch line[0] {
		case '+':
			newBuilder.WriteString(line[1:])
			newBuilder.WriteString("\n")
			continue
		case '-':
			oldBuilder.WriteString(line[1:])
			oldBuilder.WriteString("\n")
			continue
		case '\\':
			continue
		}
	}

	oldStr := strings.TrimSuffix(oldBuilder.String(), "\n")
	newStr := strings.TrimSuffix(newBuilder.String(), "\n")
	return oldStr, newStr
}

var sedFilePattern = regexp.MustCompile(`sed\s+-n\s+'[^']*'\s+([^\s]+)`)

func extractSedFilePath(script string) string {
	match := sedFilePattern.FindStringSubmatch(script)
	if len(match) < 2 {
		return ""
	}
	return strings.Trim(match[1], "\"'")
}

func extractCatRead(script, output string) (string, string) {
	lines := strings.Split(script, "\n")
	for _, line := range lines {
		trimmed := strings.TrimSpace(line)
		if !strings.HasPrefix(trimmed, "cat ") {
			continue
		}
		fields := strings.Fields(trimmed)
		if len(fields) < 2 {
			continue
		}
		path := strings.Trim(fields[1], "\"'")
		cleanOutput := output
		if idx := strings.Index(cleanOutput, "\n---"); idx != -1 {
			cleanOutput = cleanOutput[:idx]
		}
		cleanOutput = strings.TrimRight(cleanOutput, "\n")
		return path, cleanOutput
	}
	return "", ""
}

func countLines(text string) int {
	if text == "" {
		return 0
	}
	return strings.Count(text, "\n") + 1
}

// parseISOTimestamp parses an ISO timestamp into Unix milliseconds
func parseISOTimestamp(ts string) int64 {
	if ts == "" {
		return 0
	}
	formats := []string{
		"2006-01-02T15:04:05.000Z",
		time.RFC3339Nano,
		time.RFC3339,
		"2006-01-02T15:04:05Z",
	}

	for _, format := range formats {
		if t, err := time.Parse(format, ts); err == nil {
			return t.UnixNano() / int64(time.Millisecond)
		}
	}
	return 0
}

// analyzeConversations is kept for backward compatibility
func analyzeConversations(records []map[string]interface{}) CodeAnalysis {
	return analyzeClaudeConversations(records)
}

// analyzeClaudeConversations analyzes Claude-Code conversations
func analyzeClaudeConversations(records []map[string]interface{}) CodeAnalysis {
	writeDetails := make([]CodeAnalysisWriteDetail, 0, 10)
	readDetails := make([]CodeAnalysisReadDetail, 0, 20)
	editDetails := make([]CodeAnalysisApplyDiffDetail, 0, 15)
	runDetails := make([]CodeAnalysisRunCommandDetail, 0, 5)

	toolCounts := CodeAnalysisToolCalls{}
	conversationUsage := make(ConversationUsage)
	uniqueFiles := make(map[string]struct{})

	totalWriteLines := 0
	totalReadLines := 0
	totalReadCharacters := 0
	totalWriteCharacters := 0
	totalEditCharacters := 0
	totalEditLines := 0

	folderPath := ""
	gitRemoteURL := ""
	taskID := ""
	lastTimestamp := int64(0)

	for _, record := range records {
		var claudeCodeLog ClaudeCodeLog
		if err := convertMapToStruct(record, &claudeCodeLog); err != nil {
			continue
		}

		if folderPath == "" {
			folderPath = claudeCodeLog.CWD
		}
		taskID = claudeCodeLog.SessionID

		tsInt := parseISOTimestamp(claudeCodeLog.Timestamp)
		if tsInt > lastTimestamp {
			lastTimestamp = tsInt
		}

		if claudeCodeLog.Type == "assistant" && claudeCodeLog.Message != nil {
			if messageMap, ok := claudeCodeLog.Message.(map[string]interface{}); ok {
				// Process usage data
				if model, hasModel := messageMap["model"]; hasModel {
					if usage, hasUsage := messageMap["usage"]; hasUsage {
						modelStr, _ := model.(string)
						if modelStr != "" {
							processClaudeUsageData(conversationUsage, modelStr, usage)
						}
					}
				}

				if contentArray, ok := messageMap["content"].([]interface{}); ok {
					for _, item := range contentArray {
						if itemMap, ok := item.(map[string]interface{}); ok {
							if itemType, ok := itemMap["type"].(string); ok && itemType == "tool_use" {
								if name, ok := itemMap["name"].(string); ok {
									switch name {
									case "Read":
										toolCounts.Read++
									case "Write":
										toolCounts.Write++
									case "Edit":
										toolCounts.Edit++
									case "TodoWrite":
										toolCounts.TodoWrite++
									case "Bash":
										toolCounts.Bash++
										if inputMap, ok := itemMap["input"].(map[string]interface{}); ok {
											command, _ := inputMap["command"].(string)
											description, _ := inputMap["description"].(string)
											runDetails = append(runDetails, CodeAnalysisRunCommandDetail{
												CodeAnalysisDetailBase: CodeAnalysisDetailBase{
													FilePath:       claudeCodeLog.CWD,
													LineCount:      0,
													CharacterCount: len(command),
													Timestamp:      tsInt,
												},
												Command:     command,
												Description: description,
											})
										}
									}
								}
							}
						}
					}
				}
			}
		}

		if claudeCodeLog.ToolUseResult == nil {
			continue
		}

		turMap, ok := claudeCodeLog.ToolUseResult.(map[string]interface{})
		if !ok {
			continue
		}

		if turType, exists := turMap["type"]; exists && turType == "text" {
			if fileMap, ok := turMap["file"].(map[string]interface{}); ok {
				filePath, _ := fileMap["filePath"].(string)
				content, _ := fileMap["content"].(string)
				numLinesFloat, _ := fileMap["numLines"].(float64)
				numLines := int(numLinesFloat)

				readDetails = append(readDetails, CodeAnalysisReadDetail{
					CodeAnalysisDetailBase: CodeAnalysisDetailBase{
						FilePath:       filePath,
						LineCount:      numLines,
						CharacterCount: utf8.RuneCountInString(content),
						Timestamp:      tsInt,
					},
				})
				uniqueFiles[filePath] = struct{}{}
				totalReadCharacters += utf8.RuneCountInString(content)
				totalReadLines += numLines
			}
		}

		if turType, exists := turMap["type"]; exists && turType == "create" {
			filePath, _ := turMap["filePath"].(string)
			content, _ := turMap["content"].(string)
			lineCount := len(strings.Split(content, "\n"))

			writeDetails = append(writeDetails, CodeAnalysisWriteDetail{
				CodeAnalysisDetailBase: CodeAnalysisDetailBase{
					FilePath:       filePath,
					LineCount:      lineCount,
					CharacterCount: utf8.RuneCountInString(content),
					Timestamp:      tsInt,
				},
				Content: content,
			})
			uniqueFiles[filePath] = struct{}{}
			totalWriteLines += lineCount
			totalWriteCharacters += utf8.RuneCountInString(content)
		}

		if filePath, ok := turMap["filePath"].(string); ok {
			if newString, ok := turMap["newString"].(string); ok {
				oldString, _ := turMap["oldString"].(string)
				lineCount := len(strings.Split(newString, "\n"))

				editDetails = append(editDetails, CodeAnalysisApplyDiffDetail{
					CodeAnalysisDetailBase: CodeAnalysisDetailBase{
						FilePath:       filePath,
						LineCount:      lineCount,
						CharacterCount: utf8.RuneCountInString(newString),
						Timestamp:      tsInt,
					},
					OldString: oldString,
					NewString: newString,
				})
				uniqueFiles[filePath] = struct{}{}
				totalEditCharacters += utf8.RuneCountInString(newString)
				totalEditLines += lineCount
			}
		}
	}

	gitRemoteURL = getGitRemoteOriginURL(folderPath)

	record := CodeAnalysisRecord{
		TotalUniqueFiles:     len(uniqueFiles),
		TotalWriteLines:      totalWriteLines,
		TotalReadLines:       totalReadLines,
		TotalReadCharacters:  totalReadCharacters,
		TotalWriteCharacters: totalWriteCharacters,
		TotalEditCharacters:  totalEditCharacters,
		TotalEditLines:       totalEditLines,
		WriteFileDetails:     writeDetails,
		ReadFileDetails:      readDetails,
		EditFileDetails:      editDetails,
		RunCommandDetails:    runDetails,
		ToolCallCounts:       toolCounts,
		ConversationUsage:    conversationUsage,
		TaskID:               taskID,
		Timestamp:            lastTimestamp,
		FolderPath:           folderPath,
		GitRemoteURL:         gitRemoteURL,
	}

	analysis := CodeAnalysis{
		Records: []CodeAnalysisRecord{record},
	}

	return analysis
}

// analyzeCodexConversations analyzes Codex transcripts
func analyzeCodexConversations(logs []CodexLog) CodeAnalysis {
	state := codexAnalysisState{
		writeDetails:         make([]CodeAnalysisWriteDetail, 0),
		readDetails:          make([]CodeAnalysisReadDetail, 0),
		editDetails:          make([]CodeAnalysisApplyDiffDetail, 0),
		runDetails:           make([]CodeAnalysisRunCommandDetail, 0),
		toolCounts:           CodeAnalysisToolCalls{},
		uniqueFiles:          make(map[string]struct{}),
		totalWriteLines:      0,
		totalReadLines:       0,
		totalEditLines:       0,
		totalWriteCharacters: 0,
		totalReadCharacters:  0,
		totalEditCharacters:  0,
		folderPath:           "",
		gitRemote:            "",
		taskID:               "",
		lastTS:               0,
	}
	conversationUsage := make(ConversationUsage)
	currentModel := ""
	shellCalls := make(map[string]codexShellCall)

	for _, entry := range logs {
		ts := parseISOTimestamp(entry.Timestamp)
		if ts > state.lastTS {
			state.lastTS = ts
		}

		switch entry.Type {
		case "session_meta":
			if state.folderPath == "" && entry.Payload.CWD != "" {
				state.folderPath = entry.Payload.CWD
			}
			if state.taskID == "" && entry.Payload.ID != "" {
				state.taskID = entry.Payload.ID
			}
			if state.gitRemote == "" && entry.Payload.Git != nil {
				state.gitRemote = entry.Payload.Git.RepositoryURL
			}
		case "turn_context":
			if state.folderPath == "" && entry.Payload.CWD != "" {
				state.folderPath = entry.Payload.CWD
			}
			if entry.Payload.Model != "" {
				currentModel = entry.Payload.Model
			}
		case "event_msg":
			if entry.Payload.Type == "token_count" {
				if currentModel != "" && entry.Payload.Info != nil {
					processCodexUsageData(conversationUsage, currentModel, entry.Payload.Info)
				}
			}
		case "response_item":
			switch entry.Payload.Type {
			case "function_call":
				if entry.Payload.Name != "shell" {
					continue
				}
				if entry.Payload.Arguments == "" {
					continue
				}
				var args codexShellArguments
				if err := json.Unmarshal([]byte(entry.Payload.Arguments), &args); err != nil {
					continue
				}
				script := ""
				if n := len(args.Command); n > 0 {
					script = args.Command[n-1]
				}
				shellCalls[entry.Payload.CallID] = codexShellCall{
					Timestamp:   ts,
					Script:      script,
					FullCommand: args.Command,
				}
			case "function_call_output":
				callID := entry.Payload.CallID
				call, ok := shellCalls[callID]
				if !ok {
					continue
				}

				var result codexShellOutput
				if entry.Payload.Output != "" {
					if err := json.Unmarshal([]byte(entry.Payload.Output), &result); err != nil {
						result.Output = entry.Payload.Output
					}
				}
				state.handleShellCall(call, result)
				delete(shellCalls, callID)
			}
		}
	}

	if state.gitRemote == "" {
		state.gitRemote = getGitRemoteOriginURL(state.folderPath)
	}

	record := CodeAnalysisRecord{
		TotalUniqueFiles:     len(state.uniqueFiles),
		TotalWriteLines:      state.totalWriteLines,
		TotalReadLines:       state.totalReadLines,
		TotalEditLines:       state.totalEditLines,
		TotalWriteCharacters: state.totalWriteCharacters,
		TotalReadCharacters:  state.totalReadCharacters,
		TotalEditCharacters:  state.totalEditCharacters,
		WriteFileDetails:     state.writeDetails,
		ReadFileDetails:      state.readDetails,
		EditFileDetails:      state.editDetails,
		RunCommandDetails:    state.runDetails,
		ToolCallCounts:       state.toolCounts,
		ConversationUsage:    conversationUsage,
		TaskID:               state.taskID,
		Timestamp:            state.lastTS,
		FolderPath:           state.folderPath,
		GitRemoteURL:         state.gitRemote,
	}

	return CodeAnalysis{Records: []CodeAnalysisRecord{record}}
}

func getGitRemoteOriginURL(cwd string) string {
	if cwd == "" {
		return ""
	}
	cfgPath := filepath.Join(cwd, ".git", "config")
	f, err := os.Open(cfgPath)
	if err != nil {
		return ""
	}
	defer f.Close()
	scanner := bufio.NewScanner(f)
	inOrigin := false
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if strings.HasPrefix(line, "[") && strings.HasSuffix(line, "]") {
			inOrigin = strings.HasPrefix(line, "[remote \"origin\"")
			continue
		}
		if inOrigin && strings.HasPrefix(line, "url = ") {
			return strings.TrimSpace(strings.TrimPrefix(line, "url = "))
		}
	}
	return ""
}

// convertMapToStruct converts a map to struct using JSON marshaling
func convertMapToStruct(input map[string]interface{}, output interface{}) error {
	recordJSON, err := json.Marshal(input)
	if err != nil {
		return err
	}
	return json.Unmarshal(recordJSON, output)
}

// detectExtensionType detects whether the log is from Claude-Code or Codex
func detectExtensionType(data []map[string]interface{}) string {
	if len(data) == 0 {
		return "Codex"
	}

	for _, record := range data {
		if _, hasParentUuid := record["parentUuid"]; hasParentUuid {
			return "Claude-Code"
		}
	}
	return "Codex"
}

func analyzeRecordSet(data []map[string]interface{}) map[string]interface{} {
	extName := detectExtensionType(data)
	cfg := DefaultConfig(extName)

	var analysis CodeAnalysis
	if extName == "Codex" {
		logs := make([]CodexLog, 0, len(data))
		for _, record := range data {
			var entry CodexLog
			if err := convertMapToStruct(record, &entry); err != nil {
				continue
			}
			logs = append(logs, entry)
		}
		analysis = analyzeCodexConversations(logs)
	} else {
		analysis = analyzeClaudeConversations(data)
	}
	analysis.User = cfg.UserName
	analysis.ExtensionName = cfg.ExtensionName
	analysis.MachineID = cfg.MachineID
	analysis.InsightsVersion = cfg.InsightsVersion

	return map[string]interface{}{
		"user":            analysis.User,
		"records":         analysis.Records,
		"extensionName":   analysis.ExtensionName,
		"machineId":       analysis.MachineID,
		"insightsVersion": analysis.InsightsVersion,
	}
}

// AnalyzeJSONLFile analyzes a JSONL file and returns the analysis result
func AnalyzeJSONLFile(filePath string) map[string]interface{} {
	if _, err := os.Stat(filePath); os.IsNotExist(err) {
		return map[string]interface{}{}
	}
	data, err := ReadJSONL(filePath)
	if err != nil {
		return map[string]interface{}{}
	}

	return analyzeRecordSet(data)
}

// saveAnalysisLog saves log into folder for debugging
func saveAnalysisLog(result map[string]interface{}, outputPath string) ([]byte, error) {
	jsonOutput, err := json.MarshalIndent(result, "", "  ")
	if err != nil {
		return nil, err
	}
	if outputPath != "" {
		if err := os.MkdirAll(filepath.Dir(outputPath), 0755); err != nil {
			return nil, err
		}
		if err := os.WriteFile(outputPath, jsonOutput, 0644); err != nil {
			return nil, err
		}
	}
	return jsonOutput, nil
}

// AnalysisParams holds the parameters for RunAnalysis
type AnalysisParams struct {
	O11yBaseURL string
	InputPath   string
	OutputPath  string
	LogEnabled  bool
	CodexArg    string
}

// RunAnalysis performs analysis for both Claude Code and Codex
func RunAnalysis(params AnalysisParams) {
	// Step 1: Process input to get the JSONL file path
	inputSource, err := ProcessInput(params.InputPath, params.CodexArg)
	if err != nil {
		return
	}

	// Step 2: Analyze the JSONL file
	result := AnalyzeJSONLFile(inputSource.FilePath)
	if len(result) == 0 {
		os.Exit(1)
	}

	// Step 3: Handle debug logging
	var logDir string
	if params.LogEnabled {
		if p, err := ResolvePaths(); err == nil {
			ts := time.Now().Format("20060102-150405")

			baseLogDir := p.ClaudeLogDir
			if params.CodexArg != "" {
				baseLogDir = p.CodexLogDir
			} else if extName, ok := result["extensionName"].(string); ok && extName == "Codex" {
				baseLogDir = p.CodexLogDir
			}

			logDir = filepath.Join(baseLogDir, fmt.Sprintf("analysis_%s", ts))
			os.MkdirAll(logDir, 0o755)

			if params.CodexArg != "" {
				if inputSource.RawEventJSON != "" {
					os.WriteFile(filepath.Join(logDir, "event.json"), []byte(inputSource.RawEventJSON), 0o644)
				}
				if inputSource.HistoryEntry != nil {
					if b, err := json.MarshalIndent(inputSource.HistoryEntry, "", "  "); err == nil {
						os.WriteFile(filepath.Join(logDir, "history_entry.json"), b, 0o644)
					}
				}
				if sessionPath, ok := inputSource.DebugMetadata["sessionPath"].(string); ok {
					os.WriteFile(filepath.Join(logDir, "session_path.txt"), []byte(sessionPath), 0o644)
				}
			}

			saveAnalysisLog(result, filepath.Join(logDir, "parse.json"))
		}
	}

	// Step 4: Handle output file if specified
	if params.OutputPath != "" {
		saveAnalysisLog(result, params.OutputPath)
	}

	// Step 5: Send analysis data and get response
	var responseData map[string]interface{}
	if params.InputPath != "" {
		// File input mode - output result
		if jsonOutput, err := saveAnalysisLog(result, ""); err == nil {
			fmt.Println(string(jsonOutput))
		}
		return
	} else {
		// Interactive mode - send to O11y API
		responseData = SendAnalysisData(params.O11yBaseURL, result)
	}

	// Step 6: Save response debug file
	if params.LogEnabled && logDir != "" && responseData != nil {
		if b, err := json.MarshalIndent(responseData, "", "  "); err == nil {
			os.WriteFile(filepath.Join(logDir, "response.json"), b, 0o644)
		}
	}

	// Step 7: Exit
	os.Exit(0)
}

// ===== Main Function (Example Usage) =====

func main() {
	// Example 1: Analyze a JSONL file
	if len(os.Args) > 1 {
		filePath := os.Args[1]
		fmt.Printf("Analyzing file: %s\n", filePath)

		result := AnalyzeJSONLFile(filePath)
		if len(result) == 0 {
			fmt.Println("No results found or file doesn't exist")
			return
		}

		// Pretty print the result
		jsonOutput, err := json.MarshalIndent(result, "", "  ")
		if err != nil {
			fmt.Printf("Error marshaling result: %v\n", err)
			return
		}
		fmt.Println(string(jsonOutput))
	} else {
		// Example 2: Show usage information
		fmt.Println("Standalone Parser Example")
		fmt.Println("=========================")
		fmt.Println()
		fmt.Println("This is a standalone version of the telemetry parser that includes:")
		fmt.Println("- parser.go: Main parsing logic")
		fmt.Println("- usage.go: Usage statistics calculation")
		fmt.Println("- input.go: Input processing for both Claude Code and Codex")
		fmt.Println("- All dependencies (config, paths, version, logger, etc.)")
		fmt.Println()
		fmt.Println("Usage:")
		fmt.Println("  go run parser_example.go <path-to-jsonl-file>")
		fmt.Println()
		fmt.Println("Example:")
		fmt.Println("  go run parser_example.go ~/.claude/projects/my-project/conversation.jsonl")
		fmt.Println()
		fmt.Println("The output will be a JSON object containing analysis results.")
	}
}
