class Printer
  def self.decorate(io)
    class << io
      def write(msg)
        Printer.stamp(msg)
      end
    end
    io
  end

  def self.stamp(msg)
    msg
  end
end
Printer.decorate($stdout)
