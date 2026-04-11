#!/bin/bash
# aginx agent 扫描脚本
# 扫描指定目录（默认当前目录）下的 aginx.toml 文件

set -e

# 颜色定义
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# 默认扫描目录
SCAN_DIR="${1:-.}"
MAX_DEPTH="${2:-3}"

echo -e "${BLUE}=== aginx Agent Scanner ===${NC}"
echo -e "扫描目录: ${SCAN_DIR}"
echo -e "最大深度: ${MAX_DEPTH}"
echo ""

# 存储发现的 agents
declare -a FOUND_AGENTS=()

# 扫描 aginx.toml 文件
scan_for_agents() {
    local found_files
    found_files=$(find "$SCAN_DIR" -maxdepth "$MAX_DEPTH" -name "aginx.toml" -type f 2>/dev/null || true)

    if [ -z "$found_files" ]; then
        echo -e "${YELLOW}未发现任何 aginx.toml 文件${NC}"
        return
    fi

    echo -e "${GREEN}发现以下 agent 配置:${NC}"
    echo ""

    local index=1
    while IFS= read -r file; do
        local dir
        dir=$(dirname "$file")

        # 解析基本信息（简单解析 TOML）
        local id name agent_type version
        # 使用 awk 提取值，处理引号
        id=$(awk -F'=' '/^id[[:space:]]*=/ {gsub(/^[ \t"'\'']+|[ \t"'\'']+$/, "", $2); print $2; exit}' "$file" 2>/dev/null || echo "unknown")
        name=$(awk -F'=' '/^name[[:space:]]*=/ {gsub(/^[ \t"'\'']+|[ \t"'\'']+$/, "", $2); print $2; exit}' "$file" 2>/dev/null || echo "Unknown Agent")
        agent_type=$(awk -F'=' '/^agent_type[[:space:]]*=/ {gsub(/^[ \t"'\'']+|[ \t"'\'']+$/, "", $2); print $2; exit}' "$file" 2>/dev/null || echo "unknown")
        version=$(awk -F'=' '/^version[[:space:]]*=/ {gsub(/^[ \t"'\'']+|[ \t"'\'']+$/, "", $2); print $2; exit}' "$file" 2>/dev/null || echo "0.0.0")

        echo -e "  ${GREEN}[$index]${NC} ${id}"
        echo -e "      名称: ${name}"
        echo -e "      类型: ${agent_type}"
        echo -e "      版本: ${version}"
        echo -e "      路径: ${dir}"
        echo ""

        FOUND_AGENTS+=("$file|$dir|$id")
        ((index++))
    done <<< "$found_files"
}

# 验证 aginx.toml 格式
validate_config() {
    local file="$1"
    local errors=0

    # 检查必需字段
    for field in "id" "name" "agent_type"; do
        if ! grep -qE "^${field}\s*=" "$file" 2>/dev/null; then
            echo -e "  ${RED}✗ 缺少必需字段: ${field}${NC}"
            ((errors++))
        fi
    done

    return $errors
}

# 检测 agent 是否可用（运行检测命令）
check_agent_available() {
    local file="$1"
    local dir
    dir=$(dirname "$file")

    # 读取检测配置
    local check_cmd check_args
    check_cmd=$(grep -E '^\s*check_command\s*=' "$file" 2>/dev/null | sed 's/.*=\s*["'\'']//; s/["'\'']$//' | head -1)

    if [ -n "$check_cmd" ]; then
        check_args=$(grep -E '^\s*check_args\s*=' "$file" 2>/dev/null | sed 's/.*=\s*\[//; s/\].*//' | tr -d '"' | tr ',' ' ' | head -1)

        echo -e "  ${BLUE}检测命令: ${check_cmd} ${check_args}${NC}"

        # 尝试运行检测命令
        if command -v "$check_cmd" &> /dev/null; then
            if cd "$dir" && $check_cmd $check_args &> /dev/null; then
                echo -e "  ${GREEN}✓ Agent 可用${NC}"
                return 0
            else
                echo -e "  ${YELLOW}⚠ Agent 检测命令执行失败${NC}"
                return 1
            fi
        else
            echo -e "  ${YELLOW}⚠ 未找到命令: ${check_cmd}${NC}"
            return 1
        fi
    fi

    echo -e "  ${BLUE}无检测配置，跳过${NC}"
    return 0
}

# 主流程
main() {
    scan_for_agents

    if [ ${#FOUND_AGENTS[@]} -eq 0 ]; then
        echo -e "${YELLOW}提示: 在项目根目录创建 aginx.toml 文件来注册 agent${NC}"
        exit 0
    fi

    echo -e "${BLUE}===========================${NC}"
    echo ""

    # 询问是否验证
    read -p "是否验证并检测这些 agent? (y/n): " -n 1 -r
    echo ""

    if [[ $REPLY =~ ^[Yy]$ ]]; then
        echo ""
        echo -e "${BLUE}=== 验证和检测 ===${NC}"
        echo ""

        for entry in "${FOUND_AGENTS[@]}"; do
            IFS='|' read -r file dir id <<< "$entry"
            echo -e "${BLUE}检查: ${id}${NC}"

            validate_config "$file"
            check_agent_available "$file"

            echo ""
        done
    fi

    # 显示注册命令
    echo -e "${BLUE}===========================${NC}"
    echo -e "${GREEN}要注册这些 agent，请运行:${NC}"
    echo ""
    for entry in "${FOUND_AGENTS[@]}"; do
        IFS='|' read -r file dir id <<< "$entry"
        echo -e "  aginx register ${dir}"
    done
    echo ""
}

main
