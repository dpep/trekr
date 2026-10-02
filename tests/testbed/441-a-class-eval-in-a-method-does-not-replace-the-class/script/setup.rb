class Setup
  def run
    ::Registry.class_eval do
      def self.current
        :override
      end
    end
  end
end
