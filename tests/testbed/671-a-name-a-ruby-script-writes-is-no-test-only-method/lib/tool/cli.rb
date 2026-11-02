module Tool
  class CLI
    def self.start(argv)
      new.run(argv)
    end

    def run(argv)
      argv
    end

    def banner
      "tool"
    end

    def usage
      "tool ARGS"
    end
  end
end
