describe Tool::CLI do
  it { Tool::CLI.start([]); Tool::CLI.new.usage }
  it { Tool::CLI.start(["x"]); Tool::CLI.new.usage }
end
