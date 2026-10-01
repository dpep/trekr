module Cli
  class Tool < Thor
    include Shared

    desc "go", "Go"
    def go; end

    no_commands do
      def unused_helper; end
    end

    no_tasks do
      def unused_task_helper; end
    end
  end
end
