#!/bin/bash
# cleanup_repo.sh - Clean repository for sharing

echo "🧹 Cleaning repository for sharing..."

# Remove build artifacts
echo "🗑️  Removing build artifacts..."
rm -rf build/
rm -rf server/build/
rm -rf */build/
find . -name "CMakeCache.txt" -delete
find . -name "CMakeFiles" -type d -exec rm -rf {} + 2>/dev/null || true
find . -name "cmake_install.cmake" -delete
find . -name "Makefile" -delete
find . -name "gateway_server" -delete
find . -name "*.o" -delete
find . -name "*.so" -delete
find . -name "*.dylib" -delete

# Remove generated protobuf files
echo "🗑️  Removing generated protobuf files..."
rm -rf generated/
rm -f vector_service_pb2.py
rm -f vector_service_pb2_grpc.py
rm -f *_pb2.py
rm -f *_pb2_grpc.py

# Remove Python artifacts
echo "🗑️  Removing Python artifacts..."
find . -name "__pycache__" -type d -exec rm -rf {} + 2>/dev/null || true
find . -name "*.pyc" -delete
find . -name "*.pyo" -delete
rm -rf worker/.venv/
rm -rf .venv/
rm -rf venv/
rm -rf env/

# Remove IDE/system files
echo "🗑️  Removing IDE and system files..."
rm -rf .vscode/
rm -rf .idea/
find . -name "*.swp" -delete
find . -name ".DS_Store" -delete
find . -name "Thumbs.db" -delete

# Remove logs and temporary files
echo "🗑️  Removing logs and temporary files..."
find . -name "*.log" -delete
rm -f dump.rdb
rm -f *.rdb
rm -f nohup.out
rm -f *.tmp
rm -f *.temp

# Remove any other common artifacts
rm -f core
rm -f a.out
rm -f *.dmp

echo "✅ Repository cleaned!"
echo ""
echo "📋 Remaining files:"
find . -type f -not -path "./.git/*" | head -20
if [ $(find . -type f -not -path "./.git/*" | wc -l) -gt 20 ]; then
    echo "... and $(( $(find . -type f -not -path "./.git/*" | wc -l) - 20 )) more files"
fi

echo ""
echo "🎯 Repository is now ready for sharing!"
echo "📝 Don't forget to add a proper .gitignore file"
