module Widget
  def self.sql(text)
  end

  class Table
    def project
    end

  private

    # @api private
    Row = Struct.new(:cells) do
      def width
      end
    end
  end
end

module Tools
  def self.run
  end
end
