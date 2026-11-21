class Report
  def run
    def helper; 1; end; footer
  end

  def self.boot; 1; end; register

  def self.start
    1
  end; launch

  def footer; end
  def self.register; end
  def self.launch; end
end
Report.new.run
Report.new.run
