class Runner
  class << self
    class Job
      def perform = 1
      def self.build = new
    end
    def go = Job.build.perform
  end
end
Runner.go
