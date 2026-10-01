class Report
  def self.run(type)
    method = :"report_#{type}"
    public_send(method) if respond_to?(method)
  end
end
